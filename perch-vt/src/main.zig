// SPDX-License-Identifier: GPL-3.0-only

//! perch-vt: the C functions Perch needs from Ghostty's terminal library.
//! Mirrored by hand in `crates/perch/src/vt_ffi.rs`; keep the two in step.
//!
//! A model (`Vt.zig`) is a Ghostty terminal fed the program's output. It
//! answers DSR 6 with replies tagged by frame, reports whether its parser is
//! between sequences, and builds restores (`restore.zig`) from its state.
//! With `-Dtest-observer=true` it also records the queries its parser
//! dispatched, for the tracker's differential tests.
//!
//! `perch_vt_selftest` drives a throwaway terminal through the parser, a
//! DSR 6 reply, a resize and the VT formatter, so that `perch version` can
//! show the link works.

const std = @import("std");
const vt = @import("ghostty-vt");
const modes = @import("modes.zig");
const restore = @import("restore.zig");
const dump = @import("dump.zig");
const Vt = @import("Vt.zig");

const Stream = vt.TerminalStream;
const Handler = Stream.Handler;

const gpa = std.heap.smp_allocator;

/// A model as C sees it.
pub const Handle = opaque {};

fn model(h: *Handle) *Vt {
    return @ptrCast(@alignCast(h));
}

/// Bytes allocated by perch-vt, freed with `perch_vt_bytes_free`.
pub const Bytes = extern struct {
    ptr: ?[*]u8 = null,
    len: usize = 0,
};

/// Create a model with `cols` by `rows` cells (each at least 1) and a
/// scrollback limited by lines and by bytes, whichever binds first. Returns
/// null if memory runs out.
export fn perch_vt_new(cols: u16, rows: u16, scrollback_lines: u32, scrollback_bytes: u64) ?*Handle {
    const self = Vt.create(gpa, .{
        .cols = cols,
        .rows = rows,
        .scrollback_lines = scrollback_lines,
        .scrollback_bytes = std.math.cast(usize, scrollback_bytes) orelse std.math.maxInt(usize),
    }) catch return null;
    return @ptrCast(self);
}

export fn perch_vt_free(h: ?*Handle) void {
    if (h) |handle| model(handle).destroy();
}

/// Feed `len` bytes of output belonging to frame `frame`. DSR 6 replies to
/// queries in them are tagged with that frame.
export fn perch_vt_feed(h: *Handle, bytes: ?[*]const u8, len: usize, frame: u64) void {
    const data = bytes orelse return;
    model(h).feed(data[0..len], frame);
}

/// Resize the model. Returns 0 or a negative `Status`.
export fn perch_vt_resize(h: *Handle, cols: u16, rows: u16) c_int {
    model(h).resize(cols, rows) catch |err| return @backingInt(resizeStatus(err));
    return @backingInt(Status.ok);
}

fn resizeStatus(err: vt.Terminal.ResizeError) Status {
    return switch (err) {
        error.OutOfMemory => .out_of_memory,
        else => .invalid_argument,
    };
}

/// Whether the parser and its UTF-8 decoder are both at ground.
export fn perch_vt_at_ground(h: *Handle) bool {
    return model(h).atGround();
}

/// Move up to `cap` DSR 6 replies, oldest first, into `out`, and return how
/// many. Ghostty's other replies never get here.
export fn perch_vt_take_replies(h: *Handle, out: [*]Vt.Reply, cap: usize) usize {
    return model(h).takeReplies(out[0..cap]);
}

/// Build a restore into `out`: `mode` 0 for full (history, screens, state),
/// 1 for screen (no history). The model is first resized to the client's
/// size if it differs; the caller resizes the PTY after. History is the
/// newest rows that fit `client_scrollback_rows` and `history_cap` bytes;
/// everything else is limited to `byte_cap` bytes, past which the result
/// is `Status.too_large` and `out` is left empty. The prologue and, for a
/// full restore, the clear before it are the caller's.
export fn perch_vt_restore(
    h: *Handle,
    mode: u32,
    client_cols: u16,
    client_rows: u16,
    client_scrollback_rows: u32,
    history_cap: usize,
    byte_cap: usize,
    out: *Bytes,
) c_int {
    out.* = .{};
    const restore_mode = std.enums.fromInt(restore.Mode, mode) orelse
        return @backingInt(Status.invalid_argument);
    const self = model(h);
    self.resize(client_cols, client_rows) catch |err| return @backingInt(resizeStatus(err));
    const bytes = restore.build(gpa, &self.terminal, restore_mode, .{
        .client_scrollback_rows = client_scrollback_rows,
        .history_cap = history_cap,
        .byte_cap = byte_cap,
    }) catch |err| return @backingInt(switch (err) {
        error.OutOfMemory => Status.out_of_memory,
        error.TooLarge => Status.too_large,
    });
    out.* = .{ .ptr = bytes.ptr, .len = bytes.len };
    return @backingInt(Status.ok);
}

/// The last `history_rows` rows of history and the active screen as text
/// with SGR, for `tail.txt`.
export fn perch_vt_tail(h: *Handle, history_rows: u32, out: *Bytes) c_int {
    out.* = .{};
    const bytes = restore.tail(gpa, &model(h).terminal, history_rows) catch
        return @backingInt(Status.out_of_memory);
    out.* = .{ .ptr = bytes.ptr, .len = bytes.len };
    return @backingInt(Status.ok);
}

/// The model's state as JSON, for tests: both screens' cursors, saved
/// cursors and kitty stacks, every mode in the table, margins, tab stops,
/// palette and each row's text. See `dump.zig`.
export fn perch_vt_dump(h: *Handle, out: *Bytes) c_int {
    out.* = .{};
    const bytes = dump.dump(gpa, &model(h).terminal) catch
        return @backingInt(Status.out_of_memory);
    out.* = .{ .ptr = bytes.ptr, .len = bytes.len };
    return @backingInt(Status.ok);
}

export fn perch_vt_bytes_free(out: *Bytes) void {
    if (out.ptr) |ptr| gpa.free(ptr[0..out.len]);
    out.* = .{};
}

/// The window title (OSC 0 or 2), valid until the next feed. Null if unset.
export fn perch_vt_title(h: *Handle, len: *usize) ?[*]const u8 {
    return borrowed(model(h).terminal.getTitle(), len);
}

/// The working directory the program reported (OSC 7), valid until the
/// next feed. Null if unset.
export fn perch_vt_pwd(h: *Handle, len: *usize) ?[*]const u8 {
    return borrowed(model(h).terminal.getPwd(), len);
}

fn borrowed(text: ?[:0]const u8, len: *usize) ?[*]const u8 {
    const t = text orelse {
        len.* = 0;
        return null;
    };
    len.* = t.len;
    return t.ptr;
}

/// 1 if the mode is set, 0 if reset, -1 if Ghostty does not know it.
/// `ansi` selects ANSI modes (`CSI n h`) over DEC private ones.
export fn perch_vt_mode(h: *Handle, value: u16, ansi: bool) c_int {
    const mode = vt.modes.modeFromInt(value, ansi) orelse return -1;
    return @intFromBool(model(h).terminal.modes.get(mode));
}

/// 0 for the primary screen, 1 for the alternate.
export fn perch_vt_active_screen(h: *Handle) c_int {
    return switch (model(h).terminal.screens.active_key) {
        .primary => 0,
        .alternate => 1,
    };
}

/// The mode table, from which the prologue's resets are generated.
export fn perch_vt_prologue_table(len: *usize) [*]const modes.CEntry {
    len.* = modes.c_table.len;
    return &modes.c_table;
}

comptime {
    if (Vt.observer) {
        @export(&takeObserved, .{ .name = "perch_vt_take_observed" });
        @export(&feedSlice, .{ .name = "perch_vt_feed_slice" });
        @export(&testAbort, .{ .name = "perch_vt_test_abort" });
    }
}

/// Feed through the bulk path production builds use. Test-observer builds
/// only.
fn feedSlice(h: *Handle, bytes: ?[*]const u8, len: usize, frame: u64) callconv(.c) void {
    const data = bytes orelse return;
    model(h).feedSlice(data[0..len], frame);
}

/// A safety abort, as a Ghostty bug would cause, so tests can check that its
/// message reaches `supervisor.log`. Test-observer builds only.
fn testAbort() callconv(.c) noreturn {
    @panic("perch-vt test abort");
}

/// Move up to `cap` observed queries, oldest first, into `out`, and return
/// how many. Test-observer builds only.
fn takeObserved(h: *Handle, out: [*]Vt.Observed, cap: usize) callconv(.c) usize {
    return model(h).takeObserved(out[0..cap]);
}

/// The result of `perch_vt_selftest`. Mirrored by hand in
/// `crates/perch/src/vt_ffi.rs`; keep the two in step.
pub const Selftest = extern struct {
    /// Cursor position after the resize, zero-based.
    cursor_col: u32,
    cursor_row: u32,
    /// Size after the resize.
    cols: u32,
    rows: u32,
    /// 1 if Ghostty's kitty graphics support was compiled in, else 0.
    kitty_graphics: u32,
    reply_len: u32,
    formatted_len: u32,
    /// The DSR 6 reply the terminal wrote back.
    reply: [32]u8,
    /// The active screen formatted as VT, with the cursor.
    formatted: [256]u8,
};

pub const Status = enum(c_int) {
    ok = 0,
    out_of_memory = -1,
    reply_too_long = -2,
    formatted_too_long = -3,
    /// The restore would pass its byte limit.
    too_large = -4,
    invalid_argument = -5,
};

/// Feed `ab\r\n` and `ESC[6n` to an 80x24 terminal, resize it to 100x30,
/// format its screen and free it. Returns 0 and fills `out` on success, or a
/// negative `Status` on failure.
export fn perch_vt_selftest(out: *Selftest) c_int {
    return @backingInt(selftest(std.heap.smp_allocator, out));
}

const Session = struct {
    terminal: vt.Terminal,
    stream: Stream,
    reply: [32]u8 = undefined,
    reply_len: usize = 0,
    reply_overflow: bool = false,

    fn writePty(handler: *Handler, data: []const u8) void {
        const stream: *Stream = @fieldParentPtr("handler", handler);
        const self: *Session = @alignCast(@fieldParentPtr("stream", stream));
        const end = self.reply_len + data.len;
        if (end > self.reply.len) {
            self.reply_overflow = true;
            return;
        }
        @memcpy(self.reply[self.reply_len..end], data);
        self.reply_len = end;
    }
};

fn selftest(alloc: std.mem.Allocator, out: *Selftest) Status {
    out.* = std.mem.zeroes(Selftest);

    // The handler keeps a pointer to the terminal, so both live on the heap.
    const session = alloc.create(Session) catch return .out_of_memory;
    defer alloc.destroy(session);
    session.* = .{
        .terminal = vt.Terminal.init(vt.TinyIo.init.io(), alloc, .{
            .cols = 80,
            .rows = 24,
        }) catch return .out_of_memory,
        .stream = undefined,
    };
    defer session.terminal.deinit(alloc);

    var handler = session.terminal.vtHandler();
    handler.effects.write_pty = &Session.writePty;
    session.stream = .init(.{ .allocator = alloc, .handler = handler });
    defer session.stream.deinit();

    session.stream.nextSlice("ab\r\n\x1b[6n");
    if (session.reply_overflow) return .reply_too_long;

    session.terminal.resize(alloc, .{ .cols = 100, .rows = 30 }) catch
        return .out_of_memory;

    const screen = session.terminal.screens.active;
    var formatter: vt.formatter.ScreenFormatter = .init(screen, .vt);
    formatter.extra.cursor = true;
    formatter.extra.style = true;
    var writer: std.Io.Writer = .fixed(&out.formatted);
    formatter.format(&writer) catch return .formatted_too_long;

    out.cursor_col = screen.cursor.x;
    out.cursor_row = screen.cursor.y;
    out.cols = session.terminal.cols;
    out.rows = session.terminal.rows;
    out.kitty_graphics = @intFromBool(kittyGraphics());
    out.reply_len = @intCast(session.reply_len);
    @memcpy(out.reply[0..session.reply_len], session.reply[0..session.reply_len]);
    out.formatted_len = @intCast(writer.end);
    return .ok;
}

/// Whether Ghostty was built with kitty graphics. With the feature off its
/// image-loading limits are `void`.
fn kittyGraphics() bool {
    return @FieldType(vt.Terminal.Options, "kitty_image_loading_limits") != void;
}

test "selftest drives a real terminal" {
    var result: Selftest = undefined;
    try std.testing.expectEqual(Status.ok, selftest(std.testing.allocator, &result));
    try std.testing.expectEqualStrings("\x1b[2;1R", result.reply[0..result.reply_len]);
    try std.testing.expectEqual(@as(u32, 0), result.cursor_col);
    try std.testing.expectEqual(@as(u32, 1), result.cursor_row);
    try std.testing.expectEqual(@as(u32, 100), result.cols);
    try std.testing.expectEqual(@as(u32, 30), result.rows);
    const formatted = result.formatted[0..result.formatted_len];
    try std.testing.expect(std.mem.startsWith(u8, formatted, "ab"));
}

test "kitty graphics is compiled out" {
    try std.testing.expect(!kittyGraphics());
}

test "the C interface" {
    const testing = std.testing;
    const h = perch_vt_new(20, 5, 1000, 1 << 20).?;
    defer perch_vt_free(h);

    const out = "line\r\n\x1b]2;title\x07\x1b]7;file:///tmp\x07\x1b[?2004h\x1b[6n\x1b[";
    perch_vt_feed(h, out.ptr, out.len, 7);
    perch_vt_feed(h, null, 0, 8);
    try testing.expect(!perch_vt_at_ground(h));
    perch_vt_feed(h, "m".ptr, 1, 8);
    try testing.expect(perch_vt_at_ground(h));

    var replies: [4]Vt.Reply = undefined;
    try testing.expectEqual(@as(usize, 1), perch_vt_take_replies(h, &replies, replies.len));
    try testing.expectEqual(Vt.Reply{ .frame = 7, .row = 2, .col = 1 }, replies[0]);

    var len: usize = undefined;
    try testing.expectEqualStrings("title", perch_vt_title(h, &len).?[0..len]);
    try testing.expectEqualStrings("file:///tmp", perch_vt_pwd(h, &len).?[0..len]);
    try testing.expectEqual(@as(c_int, 1), perch_vt_mode(h, 2004, false));
    try testing.expectEqual(@as(c_int, 0), perch_vt_mode(h, 4, true));
    try testing.expectEqual(@as(c_int, -1), perch_vt_mode(h, 9999, false));
    try testing.expectEqual(@as(c_int, 0), perch_vt_active_screen(h));

    // A restore at another size resizes the model first.
    var bytes: Bytes = .{};
    try testing.expectEqual(@as(c_int, 0), perch_vt_restore(h, 0, 30, 6, 1000, 4 << 20, 32 << 20, &bytes));
    try testing.expect(bytes.len > 0);
    try testing.expect(std.mem.indexOf(u8, bytes.ptr.?[0..bytes.len], "line") != null);
    perch_vt_bytes_free(&bytes);
    try testing.expect(bytes.ptr == null);
    try testing.expectEqual(@as(u16, 30), model(h).terminal.cols);
    try testing.expectEqual(@as(u16, 6), model(h).terminal.rows);

    try testing.expectEqual(@backingInt(Status.too_large), perch_vt_restore(h, 1, 30, 6, 0, 0, 4, &bytes));
    try testing.expect(bytes.ptr == null);
    try testing.expectEqual(@backingInt(Status.invalid_argument), perch_vt_restore(h, 2, 30, 6, 0, 0, 4, &bytes));

    try testing.expectEqual(@as(c_int, 0), perch_vt_tail(h, 1000, &bytes));
    try testing.expectEqualStrings("line\n", bytes.ptr.?[0..bytes.len]);
    perch_vt_bytes_free(&bytes);

    try testing.expectEqual(@as(c_int, 0), perch_vt_resize(h, 0, 0));
    try testing.expectEqual(@as(u16, 1), model(h).terminal.cols);

    var n: usize = undefined;
    const table = perch_vt_prologue_table(&n);
    try testing.expectEqual(modes.table.len, n);
    try testing.expectEqual(@as(u16, 2027), table[n - 5].value);
}

test {
    _ = modes;
    _ = restore;
    _ = dump;
    _ = Vt;
    _ = @import("tests.zig");
}
