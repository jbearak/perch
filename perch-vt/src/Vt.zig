// SPDX-License-Identifier: GPL-3.0-only

//! One terminal model: a Ghostty terminal, the stream that parses output into
//! it, the DSR 6 replies it produced, and, in test-observer builds, the
//! queries the parser dispatched.

const Vt = @This();

const std = @import("std");
const Allocator = std.mem.Allocator;
const ghostty = @import("ghostty-vt");
const options = @import("perch_vt_options");

pub const observer = options.test_observer;

alloc: Allocator,
terminal: ghostty.Terminal,
stream: Stream,

/// The frame number of the bytes being fed, which tags their replies.
frame: u64 = 0,

/// DSR 6 replies not yet taken, oldest first.
replies: std.ArrayList(Reply) = .empty,

/// Bytes fed so far, for the observer's offsets.
fed: u64 = 0,

/// Queries the parser dispatched, oldest first (observer builds only).
observed: if (observer) std.ArrayList(Observed) else void =
    if (observer) .empty else {},

pub const Stream = ghostty.Stream(Handler);

/// A DSR 6 reply: the cursor position the terminal reported, one-based, and
/// the frame whose bytes held the query. Mirrored in `vt_ffi.rs`.
pub const Reply = extern struct {
    frame: u64,
    row: u32,
    col: u32,
};

/// What the test observer records. Mirrored in `vt_ffi.rs`.
pub const Observed = extern struct {
    /// An `ObservedKind` value.
    kind: u32,
    _pad: u32 = 0,
    /// The number of bytes fed up to and including the query's last byte,
    /// counted from the terminal's creation.
    end: u64,
};

pub const ObservedKind = enum(u32) {
    dsr5 = 1,
    dsr6 = 2,
    da1 = 3,
    da2 = 4,
    paste_set = 5,
    paste_reset = 6,
    paste_save = 7,
    paste_restore = 8,
};

/// Ghostty's terminal handler, wrapped so that the observer sees each
/// action before the terminal does. Production builds compile the wrapper
/// away to a plain forward.
pub const Handler = struct {
    inner: ghostty.TerminalStream.Handler,
    owner: *Vt,

    pub fn vt(
        self: *Handler,
        comptime action: ghostty.StreamAction.Tag,
        value: ghostty.StreamAction.Value(action),
    ) void {
        if (comptime observer) self.owner.observe(action, value);
        self.inner.vt(action, value);
    }

    pub fn deinit(self: *Handler) void {
        self.inner.deinit();
    }
};

pub const Options = struct {
    cols: u16,
    rows: u16,
    scrollback_lines: usize,
    scrollback_bytes: usize,
};

/// Create a model on the heap; the stream's handler points back into it.
pub fn create(alloc: Allocator, opts: Options) Allocator.Error!*Vt {
    const self = try alloc.create(Vt);
    errdefer alloc.destroy(self);
    self.* = .{
        .alloc = alloc,
        .terminal = try .init(ghostty.TinyIo.init.io(), alloc, .{
            .cols = @max(opts.cols, 1),
            .rows = @max(opts.rows, 1),
            .max_scrollback_lines = opts.scrollback_lines,
            .max_scrollback_bytes = opts.scrollback_bytes,
        }),
        .stream = undefined,
    };
    var inner = self.terminal.vtHandler();
    // Only `write_pty`: the model's other effects (bell, clipboard, titles
    // as events) belong to the terminal that is attached, not to Perch.
    inner.effects.write_pty = &writePty;
    self.stream = .init(.{
        .allocator = alloc,
        .handler = .{ .inner = inner, .owner = self },
    });
    return self;
}

pub fn destroy(self: *Vt) void {
    const alloc = self.alloc;
    self.stream.deinit();
    self.terminal.deinit(alloc);
    self.replies.deinit(alloc);
    if (comptime observer) self.observed.deinit(alloc);
    alloc.destroy(self);
}

/// Feed output bytes belonging to `frame`.
pub fn feed(self: *Vt, bytes: []const u8, frame: u64) void {
    self.frame = frame;
    if (comptime observer) {
        // One byte at a time, so each observed query knows the offset of
        // the byte that completed it.
        for (bytes) |c| {
            self.fed += 1;
            self.stream.next(c);
        }
    } else {
        self.feedSlice(bytes, frame);
    }
}

/// Feed through Ghostty's bulk path, `nextSlice`, which production builds'
/// `feed` takes. Test-observer builds use it to check that path against the
/// tracker; the queries it observes carry the chunk's end as their offset.
pub fn feedSlice(self: *Vt, bytes: []const u8, frame: u64) void {
    self.frame = frame;
    self.fed += bytes.len;
    self.stream.nextSlice(bytes);
}

pub fn resize(self: *Vt, cols: u16, rows: u16) ghostty.Terminal.ResizeError!void {
    const c = @max(cols, 1);
    const r = @max(rows, 1);
    if (c == self.terminal.cols and r == self.terminal.rows) return;
    try self.stream.handler.inner.resize(.{ .cols = c, .rows = r });
}

/// Whether the parser and its UTF-8 decoder are both between sequences.
pub fn atGround(self: *const Vt) bool {
    return self.stream.ground();
}

/// Move up to `out.len` replies, oldest first, into `out`.
pub fn takeReplies(self: *Vt, out: []Reply) usize {
    return take(Reply, &self.replies, out);
}

pub fn takeObserved(self: *Vt, out: []Observed) usize {
    if (comptime !observer) return 0;
    return take(Observed, &self.observed, out);
}

fn take(comptime T: type, list: *std.ArrayList(T), out: []T) usize {
    const n = @min(out.len, list.items.len);
    @memcpy(out[0..n], list.items[0..n]);
    list.replaceRangeAssumeCapacity(0, n, &.{});
    return n;
}

/// Ghostty writes replies to many queries. Only DSR 6 replies leave the
/// model; everything else, DECXCPR (`CSI ? r ; c ; p R`) included, is
/// dropped by shape.
fn writePty(inner: *ghostty.TerminalStream.Handler, data: []const u8) void {
    const handler: *Handler = @fieldParentPtr("inner", inner);
    const self = handler.owner;
    const pos = parseCursorReport(data) orelse return;
    self.replies.append(self.alloc, .{
        .frame = self.frame,
        .row = pos[0],
        .col = pos[1],
    }) catch {
        // A lost reply is a missed deadline for the supervisor, which it
        // already handles; the model carries on.
    };
}

/// `ESC [ <row> ; <col> R`, exactly, or null.
pub fn parseCursorReport(data: []const u8) ?[2]u32 {
    if (!std.mem.startsWith(u8, data, "\x1b[") or !std.mem.endsWith(u8, data, "R")) return null;
    const body = data[2 .. data.len - 1];
    const semi = std.mem.indexOfScalar(u8, body, ';') orelse return null;
    const row = parseNumber(body[0..semi]) orelse return null;
    const col = parseNumber(body[semi + 1 ..]) orelse return null;
    return .{ row, col };
}

fn parseNumber(digits: []const u8) ?u32 {
    if (digits.len == 0) return null;
    for (digits) |c| if (!std.ascii.isDigit(c)) return null;
    return std.fmt.parseInt(u32, digits, 10) catch null;
}

fn observe(
    self: *Vt,
    comptime action: ghostty.StreamAction.Tag,
    value: ghostty.StreamAction.Value(action),
) void {
    const kind: ObservedKind = switch (action) {
        .device_status => switch (value.request) {
            .operating_status => .dsr5,
            .cursor_position => .dsr6,
            else => return,
        },
        .device_attributes => switch (value) {
            .primary => .da1,
            .secondary => .da2,
            else => return,
        },
        .set_mode => if (value.mode == .bracketed_paste) .paste_set else return,
        .reset_mode => if (value.mode == .bracketed_paste) .paste_reset else return,
        .save_mode => if (value.mode == .bracketed_paste) .paste_save else return,
        .restore_mode => if (value.mode == .bracketed_paste) .paste_restore else return,
        else => return,
    };
    self.observed.append(self.alloc, .{
        .kind = @backingInt(kind),
        .end = self.fed,
    }) catch {};
}

const testing = std.testing;

test parseCursorReport {
    try testing.expectEqual([2]u32{ 3, 14 }, parseCursorReport("\x1b[3;14R").?);
    try testing.expect(parseCursorReport("\x1b[?3;14;1R") == null);
    try testing.expect(parseCursorReport("\x1b[0n") == null);
    try testing.expect(parseCursorReport("\x1b[;1R") == null);
    try testing.expect(parseCursorReport("\x1b[?1;2c") == null);
}
