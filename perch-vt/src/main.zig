// SPDX-License-Identifier: GPL-3.0-only

//! perch-vt: the C functions Perch needs from Ghostty's terminal library.
//!
//! M1 exports only `perch_vt_selftest`, which drives a real Ghostty terminal
//! through the parser, a DSR 6 reply, a resize and the VT formatter, so that
//! linking it into the Rust binary proves the static link works.

const std = @import("std");
const vt = @import("ghostty-vt");
const options = @import("perch_vt_options");

const Stream = vt.TerminalStream;
const Handler = Stream.Handler;

comptime {
    // Accepted so `build.rs` can pass it under `test-hooks`; the observer
    // itself arrives with the tracker's differential tests.
    _ = options.test_observer;
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
