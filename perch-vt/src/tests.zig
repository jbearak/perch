// SPDX-License-Identifier: GPL-3.0-only

//! Tests per capability the restore needs. Most feed a model, build a
//! restore, feed it to a second, fresh model (a terminal in the state the
//! prologue leaves), and compare the two. That second model is Ghostty
//! again, so these tests are self-referential; the fidelity harness in M2c
//! checks restores against xterm.js and tmux.

const std = @import("std");
const testing = std.testing;
const ghostty = @import("ghostty-vt");
const Vt = @import("Vt.zig");
const restore = @import("restore.zig");
const modes = @import("modes.zig");

const Screen = ghostty.Screen;
const Pin = ghostty.Pin;
const Style = ghostty.Style;

const unlimited: restore.Limits = .{
    .client_scrollback_rows = 10_000,
    .history_cap = 4 << 20,
    .byte_cap = 32 << 20,
};

fn newModel(cols: u16, rows: u16) !*Vt {
    return Vt.create(testing.allocator, .{
        .cols = cols,
        .rows = rows,
        .scrollback_lines = 10_000,
        .scrollback_bytes = 64 << 20,
    });
}

const Restored = struct {
    bytes: []u8,
    vt: *Vt,

    fn deinit(self: Restored) void {
        testing.allocator.free(self.bytes);
        self.vt.destroy();
    }
};

/// Build a restore of `a` and feed it to a fresh model of the same size.
fn restoreInto(a: *Vt, mode: restore.Mode, limits: restore.Limits) !Restored {
    const bytes = try restore.build(testing.allocator, &a.terminal, mode, limits);
    errdefer testing.allocator.free(bytes);
    const b = try newModel(a.terminal.cols, a.terminal.rows);
    b.feed(bytes, 0);
    return .{ .bytes = bytes, .vt = b };
}

fn primary(v: *Vt) *Screen {
    return v.terminal.screens.get(.primary).?;
}

fn alternate(v: *Vt) *Screen {
    return v.terminal.screens.get(.alternate).?;
}

fn historyRows(screen: *const Screen) usize {
    return screen.pages.total_rows - screen.pages.rows;
}

fn historyPin(screen: *const Screen, y: usize) Pin {
    return screen.pages.pin(.{ .history = .{ .y = @intCast(y) } }).?;
}

fn activePin(screen: *const Screen, y: usize) Pin {
    return screen.pages.pin(.{ .active = .{ .y = @intCast(y) } }).?;
}

/// What a cell shows, resolved out of its page.
const CellView = struct {
    text: [8]u21 = @splat(0),
    wide: ghostty.page.Cell.Wide,
    style: Style,
    bg_only: bool,
    protected: bool,
    link_uri: []const u8 = "",
    link_id: ?[]const u8 = null,

    fn of(pin: Pin, x: usize) CellView {
        const page = pin.node.page();
        const cell = page.getRowAndCell(x, pin.y).cell;
        var view: CellView = .{
            .wide = cell.wide,
            .protected = cell.protected,
            .bg_only = false,
            .style = .{},
        };
        switch (cell.content_tag) {
            .codepoint, .codepoint_grapheme => {
                // A blank cell and a space look the same; the restore
                // prints a space for a blank cell at the edge.
                const cp = cell.codepoint();
                view.text[0] = if (cp == 0) ' ' else cp;
                if (page.lookupGrapheme(cell)) |cps| {
                    for (cps, 1..) |g, i| view.text[i] = g;
                }
                if (cell.hasStyling()) view.style = page.styles.get(page.memory, cell.style_id).*;
            },
            .bg_color_palette => {
                view.bg_only = true;
                view.style = .{ .bg_color = .{ .palette = cell.content.color_palette.data } };
            },
            .bg_color_rgb => {
                view.bg_only = true;
                view.style = .{ .bg_color = .{ .rgb = .{
                    .r = cell.content.color_rgb.r,
                    .g = cell.content.color_rgb.g,
                    .b = cell.content.color_rgb.b,
                } } };
            },
        }
        if (cell.hyperlink) if (page.lookupHyperlink(cell)) |id| {
            const link = page.hyperlink_set.get(page.memory, id);
            view.link_uri = link.uri.slice(page.memory);
            view.link_id = switch (link.id) {
                .explicit => |e| e.slice(page.memory),
                .implicit => null,
            };
        };
        return view;
    }

    fn expectEqual(a: CellView, b: CellView) !void {
        try testing.expectEqualSlices(u21, &a.text, &b.text);
        try testing.expectEqual(a.wide, b.wide);
        try testing.expect(a.style.eql(b.style));
        try testing.expectEqual(a.bg_only, b.bg_only);
        try testing.expectEqual(a.protected, b.protected);
        try testing.expectEqualStrings(a.link_uri, b.link_uri);
        if (a.link_id) |id| try testing.expectEqualStrings(id, b.link_id.?) else try testing.expect(b.link_id == null);
    }
};

fn expectSameRow(a: Pin, b: Pin, cols: usize) !void {
    for (0..cols) |x| CellView.of(a, x).expectEqual(CellView.of(b, x)) catch |err| {
        std.debug.print("cells differ at column {d}\n", .{x});
        return err;
    };
}

fn rowWraps(pin: Pin) bool {
    return pin.node.page().getRow(pin.y).wrap;
}

/// The active areas match cell for cell, with their soft wraps.
fn expectSameActive(a: *const Screen, b: *const Screen) !void {
    try testing.expectEqual(a.pages.rows, b.pages.rows);
    for (0..a.pages.rows) |y| {
        expectSameRow(activePin(a, y), activePin(b, y), a.pages.cols) catch |err| {
            std.debug.print("active row {d} differs\n", .{y});
            return err;
        };
        try testing.expectEqual(rowWraps(activePin(a, y)), rowWraps(activePin(b, y)));
    }
}

/// `b`'s whole history is the newest `b`-sized part of `a`'s.
fn expectHistoryTail(a: *const Screen, b: *const Screen) !void {
    const na = historyRows(a);
    const nb = historyRows(b);
    try testing.expect(nb <= na);
    for (0..nb) |i| {
        const pa = historyPin(a, na - nb + i);
        const pb = historyPin(b, i);
        expectSameRow(pa, pb, a.pages.cols) catch |err| {
            std.debug.print("history row {d} of {d} differs\n", .{ i, nb });
            return err;
        };
        // The last history row is split from the screen even if it wraps.
        if (i + 1 < nb) try testing.expectEqual(rowWraps(pa), rowWraps(pb));
    }
}

fn expectSameCharsets(a: *const Screen.CharsetState, b: *const Screen.CharsetState) !void {
    for ([_]ghostty.CharsetSlot{ .G0, .G1, .G2, .G3 }) |slot| {
        const ca = a.charsets.get(slot);
        const cb = b.charsets.get(slot);
        // ASCII and UTF-8 print the same in a UTF-8 terminal.
        const same = ca == cb or ((ca == .utf8 or ca == .ascii) and (cb == .utf8 or cb == .ascii));
        try testing.expect(same);
    }
    try testing.expectEqual(a.gl, b.gl);
    try testing.expectEqual(a.gr, b.gr);
}

fn expectSameCursor(a: *const Screen, b: *const Screen) !void {
    try testing.expectEqual(a.cursor.x, b.cursor.x);
    try testing.expectEqual(a.cursor.y, b.cursor.y);
    try testing.expectEqual(a.cursor.pending_wrap, b.cursor.pending_wrap);
    try testing.expect(a.cursor.style.eql(b.cursor.style));
    try testing.expectEqual(a.cursor.protected, b.cursor.protected);
    try expectSameCharsets(&a.charset, &b.charset);
    if (a.cursor.hyperlink) |la| {
        const lb = b.cursor.hyperlink orelse return error.TestExpectedEqual;
        try testing.expectEqualStrings(la.uri, lb.uri);
        switch (la.id) {
            .explicit => |id| try testing.expectEqualStrings(id, lb.id.explicit),
            .implicit => try testing.expect(lb.id == .implicit),
        }
    } else try testing.expect(b.cursor.hyperlink == null);
}

fn expectSameSaved(a: *const Screen, b: *const Screen) !void {
    const sa = a.saved_cursor orelse return testing.expect(b.saved_cursor == null);
    const sb = b.saved_cursor orelse return error.TestExpectedEqual;
    try testing.expectEqual(sa.x, sb.x);
    try testing.expectEqual(sa.y, sb.y);
    try testing.expect(sa.style.eql(sb.style));
    try testing.expectEqual(sa.protected, sb.protected);
    try testing.expectEqual(sa.pending_wrap, sb.pending_wrap);
    try testing.expectEqual(sa.origin, sb.origin);
    try expectSameCharsets(&sa.charset, &sb.charset);
}

/// The 8 ring entries, oldest first, ending with the current one.
fn kittyRing(screen: *const Screen) [8]u5 {
    const stack = &screen.kitty_keyboard;
    var ring: [8]u5 = undefined;
    for (&ring, 1..) |*r, i| r.* = stack.flags[(@as(usize, stack.idx) + i) % 8].int();
    return ring;
}

fn expectSameScreen(a: *const Screen, b: *const Screen) !void {
    try expectSameActive(a, b);
    try expectSameCursor(a, b);
    try expectSameSaved(a, b);
    try testing.expectEqual(kittyRing(a), kittyRing(b));
}

/// Every replayed mode, the effective mouse state, and the rest of the
/// terminal state step 7 writes.
fn expectSameState(a: *const ghostty.Terminal, b: *const ghostty.Terminal) !void {
    for (modes.table) |e| {
        if (e.replay == .no) continue;
        switch (e.mode) {
            .mouse_event_x10, .mouse_event_normal, .mouse_event_button, .mouse_event_any => continue,
            .mouse_format_utf8, .mouse_format_sgr, .mouse_format_urxvt, .mouse_format_sgr_pixels => continue,
            else => {},
        }
        testing.expectEqual(a.modes.get(e.mode), b.modes.get(e.mode)) catch |err| {
            std.debug.print("mode {s} differs\n", .{@tagName(e.mode)});
            return err;
        };
    }
    try testing.expectEqual(a.flags.mouse_event, b.flags.mouse_event);
    try testing.expectEqual(a.flags.mouse_format, b.flags.mouse_format);
    try testing.expectEqual(a.flags.modify_other_keys_2, b.flags.modify_other_keys_2);
    try testing.expectEqual(a.scrolling_region, b.scrolling_region);
    for (0..a.cols) |col| try testing.expectEqual(a.tabstops.get(col), b.tabstops.get(col));
    try testing.expectEqual(a.cursor.is_default, b.cursor.is_default);
    try testing.expectEqual(a.screens.active.cursor.cursor_style, b.screens.active.cursor.cursor_style);
    try testing.expectEqualSlices(u8, std.mem.asBytes(&a.colors.palette.current), std.mem.asBytes(&b.colors.palette.current));
    try testing.expect(a.colors.palette.mask.eql(b.colors.palette.mask));
    try testing.expectEqual(a.colors.foreground.override, b.colors.foreground.override);
    try testing.expectEqual(a.colors.background.override, b.colors.background.override);
    try testing.expectEqual(a.colors.cursor.override, b.colors.cursor.override);
    try testing.expectEqualStrings(a.getTitle() orelse "", b.getTitle() orelse "");
    try testing.expectEqualStrings(a.getPwd() orelse "", b.getPwd() orelse "");
    try testing.expectEqual(a.screens.active_key, b.screens.active_key);
}

fn expectSameTerminal(a: *Vt, b: *Vt) !void {
    try expectSameScreen(primary(a), primary(b));
    if (a.terminal.screens.active_key == .alternate) try expectSameScreen(alternate(a), alternate(b));
    try expectSameState(&a.terminal, &b.terminal);
}

/// Feed the same bytes to both.
fn feedBoth(a: *Vt, b: *Vt, bytes: []const u8) void {
    a.feed(bytes, 0);
    b.feed(bytes, 0);
}

/// Where the screens start: the first `ESC [H ESC [2J`.
fn screenStart(bytes: []const u8) usize {
    return std.mem.indexOf(u8, bytes, "\x1b[H\x1b[2J").?;
}

test "history: soft wraps joined, at the client's width" {
    const a = try newModel(10, 3);
    defer a.destroy();
    a.feed("first\r\n", 0);
    a.feed("abcdefghijKLMNOPQRSTuv\r\n", 0); // wraps over three rows
    for (0..5) |i| a.feed(([_]u8{ 'l', '0' + @as(u8, @intCast(i)), '\r', '\n' })[0..], 0);
    a.feed("prompt$ ", 0);
    try testing.expect(historyRows(primary(a)) >= 6);

    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    const history = r.bytes[0..screenStart(r.bytes)];
    // Joined: the terminal wraps the logical line itself.
    try testing.expect(std.mem.indexOf(u8, history, "abcdefghijKLMNOPQRSTuv") != null);
    try testing.expectEqual(historyRows(primary(a)), historyRows(primary(r.vt)));
    try expectHistoryTail(primary(a), primary(r.vt));
    try expectSameTerminal(a, r.vt);

    // Output after the restore lands the same.
    feedBoth(a, r.vt, "echo hi\r\nhi\r\n");
    try expectHistoryTail(primary(a), primary(r.vt));
    try expectSameTerminal(a, r.vt);
}

test "history: a line wrapping from history onto the screen is split there" {
    const a = try newModel(10, 3);
    defer a.destroy();
    a.feed("x\r\ny\r\n0123456789ABCDEFGHIJabc\r\nq\r\nr", 0);
    // The logical line starts in history and ends on the screen.
    const last = historyPin(primary(a), historyRows(primary(a)) - 1);
    try testing.expect(rowWraps(last));
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectHistoryTail(primary(a), primary(r.vt));
    try testing.expect(!rowWraps(historyPin(primary(r.vt), historyRows(primary(r.vt)) - 1)));
    try expectSameScreen(primary(a), primary(r.vt));
}

test "history: styles, wide characters and graphemes" {
    const a = try newModel(12, 2);
    defer a.destroy();
    a.feed("\x1b[1;31mbold red\x1b[0m \x1b[4:3;58;2;1;2;3mcurly\x1b[0m\r\n", 0);
    a.feed("\x1b[38;5;200;48;2;10;20;30m日本\x1b[0me\u{301}x\r\n", 0);
    a.feed("\x1b[3;9;53;7;2;5;8mall\x1b[0m\r\n", 0);
    a.feed("\x1b[1mbold pen", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectHistoryTail(primary(a), primary(r.vt));
    try testing.expectEqual(historyRows(primary(a)), historyRows(primary(r.vt)));
    try expectSameTerminal(a, r.vt);
}

test "history: trailing blank rows and background-only rows kept" {
    const a = try newModel(8, 3);
    defer a.destroy();
    a.feed("top\r\n\x1b[44m\x1b[2K\x1b[0m\r\n\r\nmid\x1b[42m\x1b[3X\x1b[0m\r\n\r\n\r\n", 0);
    a.feed("\r\n\r\n\r\n", 0); // the blank rows above scroll into history
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try testing.expectEqual(historyRows(primary(a)), historyRows(primary(r.vt)));
    try expectHistoryTail(primary(a), primary(r.vt));
    try expectSameTerminal(a, r.vt);
    // The background-only row is still background-only.
    try testing.expect(CellView.of(historyPin(primary(r.vt), 1), 3).bg_only);
}

test "history: trimmed by rows at the client's width and by bytes" {
    const a = try newModel(10, 3);
    defer a.destroy();
    for (0..40) |i| {
        var buf: [32]u8 = undefined;
        a.feed(try std.fmt.bufPrint(&buf, "\x1b[3{d}mline {d}\x1b[0m\r\n", .{ i % 8, i }), 0);
    }
    a.feed("long line that wraps\r\n", 0);
    const total = historyRows(primary(a));

    // By rows.
    {
        var limits = unlimited;
        limits.client_scrollback_rows = 7;
        const r = try restoreInto(a, .full, limits);
        defer r.deinit();
        try testing.expectEqual(@as(usize, 7), historyRows(primary(r.vt)));
        try expectHistoryTail(primary(a), primary(r.vt));
        try expectSameScreen(primary(a), primary(r.vt));
    }

    // By bytes: the newest rows that fit, and no more than the cap.
    {
        var limits = unlimited;
        limits.history_cap = 100;
        const r = try restoreInto(a, .full, limits);
        defer r.deinit();
        const kept = historyRows(primary(r.vt));
        try testing.expect(kept > 0 and kept < total);
        const history_len = screenStart(r.bytes) - 2 * a.terminal.rows;
        try testing.expect(history_len <= 100);
        try expectHistoryTail(primary(a), primary(r.vt));
        try expectSameScreen(primary(a), primary(r.vt));

        // One more row would not have fit.
        var bigger = limits;
        bigger.history_cap = history_len;
        const same = try restoreInto(a, .full, bigger);
        defer same.deinit();
        try testing.expectEqual(kept, historyRows(primary(same.vt)));
    }

    // A screen restore has no history at all.
    {
        const r = try restoreInto(a, .screen, unlimited);
        defer r.deinit();
        try testing.expectEqual(@as(usize, 0), historyRows(primary(r.vt)));
        try expectSameScreen(primary(a), primary(r.vt));
    }
}

test "primary screen restored while the alternate is active (1049)" {
    const a = try newModel(20, 4);
    defer a.destroy();
    a.feed("line one\r\nline two\r\n$ vim", 0);
    a.feed("\x1b[?1049h\x1b[2J\x1b[H~ vim screen ~\r\n\x1b[7m-- INSERT --\x1b[0m\x1b[1;3H", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try testing.expectEqual(.alternate, r.vt.terminal.screens.active_key);
    try expectSameTerminal(a, r.vt);

    // The program leaves with its own mode and finds its primary screen.
    feedBoth(a, r.vt, "\x1b[?1049l");
    try expectSameScreen(primary(a), primary(r.vt));
    feedBoth(a, r.vt, "\r\n$ ");
    try expectSameScreen(primary(a), primary(r.vt));
}

test "primary screen restored while the alternate is active (47 and 1047)" {
    for ([_][]const u8{ "47", "1047" }) |m| {
        const a = try newModel(20, 4);
        defer a.destroy();
        a.feed("primary\r\n\x1b[1;32msaved here\x1b7\x1b[0m more\r\nlast", 0);
        var buf: [64]u8 = undefined;
        a.feed(try std.fmt.bufPrint(&buf, "\x1b[?{s}h", .{m}), 0);
        a.feed("\x1b[2J\x1b[Halt\x1b[4mx\x1b7\x1b[0m\x1b[3;5H", 0);
        const r = try restoreInto(a, .full, unlimited);
        defer r.deinit();
        try expectSameTerminal(a, r.vt);

        feedBoth(a, r.vt, try std.fmt.bufPrint(&buf, "\x1b[?{s}l", .{m}));
        try expectSameScreen(primary(a), primary(r.vt));
        feedBoth(a, r.vt, "\x1b8after");
        try expectSameScreen(primary(a), primary(r.vt));
        // And back to the alternate screen, which keeps its saved cursor.
        feedBoth(a, r.vt, try std.fmt.bufPrint(&buf, "\x1b[?{s}h\x1b8!", .{m}));
        try expectSameCursor(alternate(a), alternate(r.vt));
    }
}

test "alternate screen drawn in ASCII when the primary kept DEC graphics" {
    const a = try newModel(20, 4);
    defer a.destroy();
    a.feed("\x1b(0", 0);
    a.feed("\x1b[?47h\x1b(B\x1b[44mqqq\x1b[0m", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectSameScreen(alternate(a), alternate(r.vt));
}

test "state without content through the mode table" {
    const a = try newModel(20, 6);
    defer a.destroy();
    a.feed("text\r\n", 0);
    a.feed("\x1b[?1h\x1b[?2004h\x1b[?1003h\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[?7l\x1b[?45h", 0);
    a.feed("\x1b[?2026h\x1b[?1007l\x1b[?2027h\x1b[?66h\x1b[?1004h\x1b[20h", 0);
    a.feed("\x1b[3g\x1b[4G\x1bH\x1b[11G\x1bH", 0); // tab stops at 3 and 10
    a.feed("\x1b[>4;2m\x1b[5 q", 0);
    a.feed("\x1b[?69h\x1b[2;15s\x1b[2;5r\x1b[?6h\x1b[2;3H", 0);
    a.feed("\x1b[4h", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);

    // 2027 first, before any content.
    try testing.expect(std.mem.startsWith(u8, r.bytes, "\x1b[?2027h"));
    // Never replayed: synchronized output and alternate scroll.
    try testing.expect(std.mem.indexOf(u8, r.bytes, "2026") == null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "1007") == null);
    try testing.expect(r.vt.terminal.modes.get(.mouse_alternate_scroll));
    try testing.expect(!r.vt.terminal.modes.get(.synchronized_output));
    // The effective mouse mode only.
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b[?1003h") == null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b[?1000h") != null);

    // Typing after the restore lands the same, in insert and origin mode.
    feedBoth(a, r.vt, "ab\x1b[Hc\r\n\x1b[5;30Hd");
    try expectSameTerminal(a, r.vt);
}

test "state: nothing extra when the program changed nothing" {
    const a = try newModel(20, 3);
    defer a.destroy();
    a.feed("$ ", 0);
    const r = try restoreInto(a, .screen, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);
    for ([_][]const u8{ "\x1b[?", "\x1b]4;", "\x1b]1", "\x1b[3g", "\x1b[>", "r\x1b", "\x1b]2;", "\x1b]7;" }) |seq| {
        testing.expect(std.mem.indexOf(u8, r.bytes, seq) == null) catch |err| {
            std.debug.print("unexpected {f} in {f}\n", .{ std.zig.fmtString(seq), std.zig.fmtString(r.bytes) });
            return err;
        };
    }
}

test "only changed palette entries and OSC 10, 11 and 12" {
    const a = try newModel(10, 2);
    defer a.destroy();
    a.feed("\x1b]4;1;rgb:12/34/56\x1b\\\x1b]4;200;#abcdef\x07\x1b]11;rgb:01/02/03\x1b\\hi", 0);
    const r = try restoreInto(a, .screen, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);
    try testing.expectEqual(@as(usize, 2), std.mem.count(u8, r.bytes, "\x1b]4;"));
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]4;1;rgb:12/34/56\x1b\\") != null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]11;rgb:01/02/03\x1b\\") != null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]10;") == null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]12;") == null);

    a.feed("\x1b]10;rgb:ff/00/00\x1b\\\x1b]12;rgb:00/ff/00\x1b\\\x1b]104;1\x1b\\", 0);
    const r2 = try restoreInto(a, .screen, unlimited);
    defer r2.deinit();
    try expectSameTerminal(a, r2.vt);
    try testing.expectEqual(@as(usize, 1), std.mem.count(u8, r2.bytes, "\x1b]4;"));
}

test "saved cursors on both screens, in the design's order" {
    const a = try newModel(10, 4);
    defer a.destroy();
    // Primary: saved cursor with a pen, protection, DEC special in G1 and
    // origin mode, then a different live state.
    a.feed("\x1b[2;4H\x1b[1;35m\x1b[1\"q\x1b)0\x0e\x1b[?6h\x1b[2;4H\x1b7", 0);
    a.feed("\x1b[?6l\x1b[0m\x1b[0\"q\x0f\x1b)B\x1b[3;1Hlive", 0);
    // Alternate (47 saves no cursor): its own saved cursor.
    a.feed("\x1b[?47h\x1b[4;2H\x1b[3m\x1b7\x1b[0m\x1b[1;1Halt", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);

    // Each saved cursor is written while its screen is active: the
    // primary's before `?47h`, the alternate's after.
    const switch_at = std.mem.indexOf(u8, r.bytes, "\x1b[?47h").?;
    try testing.expectEqual(@as(usize, 1), std.mem.count(u8, r.bytes[0..switch_at], "\x1b7"));
    try testing.expectEqual(@as(usize, 1), std.mem.count(u8, r.bytes[switch_at..], "\x1b7"));
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b8") == null);

    feedBoth(a, r.vt, "\x1b8A\x1b[?47l\x1b8B");
    try expectSameTerminal(a, r.vt);
}

test "pending wrap at a blank edge, live and saved, without DECSC" {
    // Live and saved both wait to wrap over a blank edge: DECRC brings
    // the wrap back after the edge was erased.
    {
        const a = try newModel(10, 3);
        defer a.destroy();
        a.feed("\x1b[3;1H\x1b[1mabcdefghij\x1b7\x1b[3;10H\x1b[X\x1b8", 0);
        const cursor = primary(a).cursor;
        try testing.expect(cursor.pending_wrap and cursor.x == 9);
        try testing.expect(!cursor.page_cell.hasText());
        const r = try restoreInto(a, .screen, unlimited);
        defer r.deinit();
        try expectSameScreen(primary(a), primary(r.vt));
        // Pending wrap is rebuilt by printing, never with DECSC/DECRC.
        try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b8") == null);
        try testing.expectEqual(@as(usize, 1), std.mem.count(u8, r.bytes, "\x1b7"));
        feedBoth(a, r.vt, "X\x1b8Y");
        try expectSameScreen(primary(a), primary(r.vt));
    }
    // Only the saved cursor waits to wrap over a blank edge.
    {
        const a = try newModel(10, 3);
        defer a.destroy();
        a.feed("\x1b[1m0123456789\x1b7\x1b[0m\x1b[1;10H\x1b[X\x1b[3;1Hlive", 0);
        try testing.expect(primary(a).saved_cursor.?.pending_wrap);
        const r = try restoreInto(a, .full, unlimited);
        defer r.deinit();
        try expectSameScreen(primary(a), primary(r.vt));
        feedBoth(a, r.vt, "\x1b8Y");
        try expectSameScreen(primary(a), primary(r.vt));
    }
}

test "pending wrap at a DEC special character at the edge" {
    // The cell holds the line-drawing character; the live charset is
    // still DEC special.
    {
        const a = try newModel(10, 3);
        defer a.destroy();
        a.feed("\x1b(0\x1b[1;10Hq", 0);
        try testing.expect(primary(a).cursor.pending_wrap);
        const r = try restoreInto(a, .screen, unlimited);
        defer r.deinit();
        try expectSameScreen(primary(a), primary(r.vt));
        feedBoth(a, r.vt, "qx");
        try expectSameScreen(primary(a), primary(r.vt));
    }
    // The cell holds a plain `q`, printed before DEC special was chosen:
    // the restore must print it under the clean slate.
    {
        const a = try newModel(10, 3);
        defer a.destroy();
        a.feed("\x1b[1;10Hq\x1b(0", 0);
        const r = try restoreInto(a, .screen, unlimited);
        defer r.deinit();
        try expectSameScreen(primary(a), primary(r.vt));
        try testing.expectEqual(@as(u21, 'q'), CellView.of(activePin(primary(r.vt), 0), 9).text[0]);
        feedBoth(a, r.vt, "q");
        try expectSameScreen(primary(a), primary(r.vt));
    }
    // A saved cursor waiting to wrap after a DEC special character.
    {
        const a = try newModel(10, 3);
        defer a.destroy();
        a.feed("\x1b(0\x1b[2;10Hq\x1b7\x1b(B\x1b[1;1Hok", 0);
        const r = try restoreInto(a, .screen, unlimited);
        defer r.deinit();
        try expectSameScreen(primary(a), primary(r.vt));
        feedBoth(a, r.vt, "\x1b8qq");
        try expectSameScreen(primary(a), primary(r.vt));
    }
}

test "pending wrap at a wide character at the edge" {
    const a = try newModel(10, 3);
    defer a.destroy();
    a.feed("\x1b[1;9H\x1b[32m日", 0);
    try testing.expect(primary(a).cursor.pending_wrap);
    const r = try restoreInto(a, .screen, unlimited);
    defer r.deinit();
    try expectSameScreen(primary(a), primary(r.vt));
    feedBoth(a, r.vt, "x");
    try expectSameScreen(primary(a), primary(r.vt));
}

test "kitty keyboard ring rebuilt with zeros, after wraparound" {
    const a = try newModel(10, 3);
    defer a.destroy();
    // Primary: ten pushes, zeros among them, so the ring has wrapped.
    for ([_]u8{ 1, 0, 3, 0, 5, 7, 0, 2, 31, 4 }) |f| {
        var buf: [16]u8 = undefined;
        a.feed(try std.fmt.bufPrint(&buf, "\x1b[>{d}u", .{f}), 0);
    }
    a.feed("\x1b[<1u", 0);
    // Alternate: a short stack of its own.
    a.feed("\x1b[?1049h\x1b[>5u\x1b[>0u\x1b[=9;1u", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);

    // Later pops and pushes give what they would have in the model.
    for ([_][]const u8{ "\x1b[<1u", "\x1b[<1u", "\x1b[>6u", "\x1b[<2u" }) |seq| {
        feedBoth(a, r.vt, seq);
        try testing.expectEqual(kittyRing(alternate(a)), kittyRing(alternate(r.vt)));
    }
    feedBoth(a, r.vt, "\x1b[?1049l");
    for ([_][]const u8{ "\x1b[<1u", "\x1b[<3u", "\x1b[>8u", "\x1b[<4u", "\x1b[<1u" }) |seq| {
        feedBoth(a, r.vt, seq);
        try testing.expectEqual(kittyRing(primary(a)), kittyRing(primary(r.vt)));
    }
}

test "kitty keyboard: nothing written when every entry is zero" {
    const a = try newModel(10, 3);
    defer a.destroy();
    a.feed("\x1b[>0u\x1b[>0u", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b[>") == null);
}

/// Whether every OSC 8 opened before `end` is closed by then.
fn linksClosedBefore(bytes: []const u8, end: usize) bool {
    var open = false;
    var i: usize = 0;
    while (std.mem.indexOfPos(u8, bytes[0..end], i, "\x1b]8;")) |at| {
        open = !std.mem.startsWith(u8, bytes[at..], "\x1b]8;;\x1b\\");
        i = at + 1;
    }
    return !open;
}

test "hyperlinks per cell, each region closed" {
    const a = try newModel(12, 3);
    defer a.destroy();
    a.feed("\x1b]8;;http://a\x1b\\link\x1b]8;;\x1b\\ plain\r\n", 0);
    a.feed("\x1b]8;id=x;http://b\x1b\\wrapping link text\x1b]8;;\x1b\\\r\n\r\n\r\n", 0);
    a.feed("\x1b]8;;http://c\x1b\\on screen\x1b]8;;\x1b\\\r\n\x1b]8;id=y;http://d\x1b\\open", 0);
    a.feed("\x1b[?1049h\x1b]8;;http://e\x1b\\alt\x1b]8;;\x1b\\", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectHistoryTail(primary(a), primary(r.vt));
    try expectSameTerminal(a, r.vt);

    // Before each region ends, its links are closed: history before the
    // newlines that scroll it, each screen before the next region.
    const first_screen = screenStart(r.bytes);
    try testing.expect(linksClosedBefore(r.bytes, first_screen));
    const alt_at = std.mem.indexOf(u8, r.bytes, "\x1b[?47h").?;
    try testing.expect(linksClosedBefore(r.bytes, alt_at));
}

test "one DSR 6 reply per query frame; other replies dropped" {
    const a = try newModel(20, 5);
    defer a.destroy();
    a.feed("ab\x1b[6n", 1);
    a.feed("\x1b[5n\x1b[c\x1b[>c\x1b[=c\x1b[?6n\x1b]11;?\x1b\\\x1b[?2004$p\x1b[18t\x1b[>q\x1b[?u", 2);
    a.feed("\r\n\x1b[6n", 3);
    a.feed("\x1b[2;5r\x1b[?6h\x1b[2;3H\x1b[6n\x1b[?6l", 4);
    var replies: [8]Vt.Reply = undefined;
    const n = a.takeReplies(&replies);
    try testing.expectEqual(@as(usize, 3), n);
    try testing.expectEqual(Vt.Reply{ .frame = 1, .row = 1, .col = 3 }, replies[0]);
    try testing.expectEqual(Vt.Reply{ .frame = 3, .row = 2, .col = 1 }, replies[1]);
    // In origin mode the report counts from the scroll region.
    try testing.expectEqual(Vt.Reply{ .frame = 4, .row = 2, .col = 3 }, replies[2]);
    try testing.expectEqual(@as(usize, 0), a.takeReplies(&replies));

    // Taken in pieces, oldest first.
    a.feed("\x1b[6n\x1b[6n", 5);
    try testing.expectEqual(@as(usize, 1), a.takeReplies(replies[0..1]));
    try testing.expectEqual(@as(usize, 1), a.takeReplies(replies[1..]));
    try testing.expectEqual(@as(u64, 5), replies[1].frame);
}

test "at_ground after splits inside CSI, OSC, DCS, ESC and UTF-8" {
    const a = try newModel(20, 5);
    defer a.destroy();
    try testing.expect(a.atGround());
    for ([_][2][]const u8{
        .{ "\x1b[3", "1m" },
        .{ "\x1b", "7" },
        .{ "\x1b[?20", "04h" },
        .{ "\x1b]2;tit", "le\x07" },
        .{ "\x1b]8;;http://x", "\x1b\\" },
        .{ "\x1bP$q", "m\x1b\\" },
        .{ "\x1b_Gx", "\x1b\\" },
        .{ "\xe2\x94", "\x80" },
        .{ "\xf0", "\x9f\x98\x80" },
    }) |pair| {
        a.feed(pair[0], 0);
        testing.expect(!a.atGround()) catch |err| {
            std.debug.print("at ground after {f}\n", .{std.zig.fmtString(pair[0])});
            return err;
        };
        a.feed(pair[1], 0);
        try testing.expect(a.atGround());
    }
}

test "tail: history and screen as text with SGR" {
    const a = try newModel(10, 4);
    defer a.destroy();
    for (0..6) |i| {
        var buf: [16]u8 = undefined;
        a.feed(try std.fmt.bufPrint(&buf, "old {d}\r\n", .{i}), 0);
    }
    a.feed("\x1b[1mbold\x1b[0m\r\nwrapped line here\r\n$ ", 0);
    const text = try restore.tail(testing.allocator, &a.terminal, 2);
    defer testing.allocator.free(text);
    try testing.expectEqualStrings(
        "old 4\nold 5\n\x1b[0m\x1b[1mbold\x1b[0m\nwrapped line here\n$ \n",
        text,
    );
    // No terminal state, no cursor movement but stepping over blanks.
    try testing.expect(std.mem.indexOf(u8, text, "\x1b[H") == null);

    const all = try restore.tail(testing.allocator, &a.terminal, 1000);
    defer testing.allocator.free(all);
    try testing.expect(std.mem.startsWith(u8, all, "old 0\n"));

    const empty = try newModel(10, 4);
    defer empty.destroy();
    const none = try restore.tail(testing.allocator, &empty.terminal, 1000);
    defer testing.allocator.free(none);
    try testing.expectEqualStrings("", none);
}

test "title and pwd" {
    const a = try newModel(10, 2);
    defer a.destroy();
    a.feed("\x1b]2;R console\x1b\\\x1b]7;file://host/home/u\x07", 0);
    try testing.expectEqualStrings("R console", a.terminal.getTitle().?);
    try testing.expectEqualStrings("file://host/home/u", a.terminal.getPwd().?);
    const r = try restoreInto(a, .screen, unlimited);
    defer r.deinit();
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]2;R console\x1b\\") != null);
    try testing.expect(std.mem.indexOf(u8, r.bytes, "\x1b]7;file://host/home/u\x1b\\") != null);
    try expectSameState(&a.terminal, &r.vt.terminal);
}

test "insert mode does not shift the edge cell rebuilt for pending wrap" {
    const a = try newModel(10, 3);
    defer a.destroy();
    a.feed("0123456789\x1b[4h", 0);
    const r = try restoreInto(a, .screen, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);
    feedBoth(a, r.vt, "X\x1b[1;3HY");
    try expectSameTerminal(a, r.vt);
}

test "without wraparound, the edge cell rebuilt for pending wrap keeps its combining mark" {
    const a = try newModel(2, 1);
    defer a.destroy();
    a.feed("\te\xcc\x81\x1b[?7l", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectSameTerminal(a, r.vt);
}

test "history: a wrap onto a row with a background does not color the scrolled-in row" {
    const a = try newModel(3, 2);
    defer a.destroy();
    // "lqk" wraps onto a row whose first cell has a background; that row
    // then scrolls into history.
    a.feed("word \xe4\xb8\xadlqk\x1b[48;2;1;2;3m a\x1b[0m\r\nx\r\ny", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    try expectHistoryTail(primary(a), primary(r.vt));
    try expectSameTerminal(a, r.vt);
}

test "joins: a spacer head is followed by its wide character, or no join" {
    // With left and right margins, the row after one ending where a wide
    // character did not fit can start with older text.
    const a = try newModel(4, 3);
    defer a.destroy();
    a.feed("\x1b[?69h0123456789\x1b[3;9s\t\xe4\xb8\xad\xe4\xb8\xad", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    // The next row's text stays on its own row, not in the spacer's column
    // (which the restore leaves blank rather than as a spacer head).
    for (0..3) |y| {
        var ita = activePin(primary(a), y).rowIterator(.right_down, null);
        var itb = activePin(primary(r.vt), y).rowIterator(.right_down, null);
        const ra = ita.next().?;
        const rb = itb.next().?;
        for (0..4) |x| {
            const ca = ra.node.page().getRowAndCell(x, ra.y).cell;
            const cb = rb.node.page().getRowAndCell(x, rb.y).cell;
            try testing.expectEqual(ca.codepoint(), cb.codepoint());
        }
    }
}

test "joins: a row the formatter skips is never joined" {
    // A row holding only a styled blank (a wide character destroyed after
    // it did not fit) is written as nothing.
    const a = try newModel(2, 2);
    defer a.destroy();
    a.feed("\x1b[1mword \x00\x1b[10C\x1b[1K\xe4\xb8\xad\re\xcc\x81", 0);
    const r = try restoreInto(a, .full, unlimited);
    defer r.deinit();
    const b = primary(r.vt);
    var it = activePin(b, 1).rowIterator(.right_down, null);
    const row = it.next().?;
    const cell = row.node.page().getRowAndCell(0, row.y).cell;
    try testing.expectEqual(@as(u21, 'e'), cell.codepoint());
}

test "the byte cap" {
    const a = try newModel(40, 10);
    defer a.destroy();
    for (0..10) |_| a.feed("\x1b[31mred\x1b[32mgreen\x1b[33myellow\x1b[0m\r\n", 0);
    var limits = unlimited;
    limits.byte_cap = 64;
    try testing.expectError(error.TooLarge, restore.build(testing.allocator, &a.terminal, .screen, limits));
    // History counts against its own cap, not this one.
    limits.byte_cap = 1 << 20;
    const ok = try restore.build(testing.allocator, &a.terminal, .full, limits);
    testing.allocator.free(ok);
}

test "observer records queries with the offsets where they end" {
    const a = try newModel(20, 5);
    defer a.destroy();
    a.feed("ab\x1b[5n\x1b[", 0);
    a.feed("6n\x1b[c\x1b[>c\x1b[=c\x1b[?6n", 1);
    a.feed("\x1b[?2004h\x1b[?2004l\x1b[?2004s\x1b[?2004r\x1b[?1000;2004h", 2);
    var observed: [16]Vt.Observed = undefined;
    const n = a.takeObserved(&observed);
    if (!Vt.observer) {
        try testing.expectEqual(@as(usize, 0), n);
        return;
    }
    const K = Vt.ObservedKind;
    const expected = [_]struct { K, u64 }{
        .{ .dsr5, 6 },
        .{ .dsr6, 10 },
        .{ .da1, 13 },
        .{ .da2, 17 },
        .{ .paste_set, 34 },
        .{ .paste_reset, 42 },
        .{ .paste_save, 50 },
        .{ .paste_restore, 58 },
        .{ .paste_set, 71 },
    };
    try testing.expectEqual(expected.len, n);
    for (expected, observed[0..n]) |e, o| {
        try testing.expectEqual(@backingInt(e[0]), o.kind);
        try testing.expectEqual(e[1], o.end);
    }
    try testing.expectEqual(@as(usize, 0), a.takeObserved(&observed));
}
