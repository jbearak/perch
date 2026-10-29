// SPDX-License-Identifier: GPL-3.0-only

//! Builds a restore from a model's state, and the text for `tail.txt`.
//!
//! A restore is what follows the prologue (and, for a full restore, the
//! clear of step 2) that Perch's Rust side writes. It assumes the defaults
//! the prologue sets and writes only what differs from them. In order, as
//! "Restore on attach" in docs/design.md describes:
//!
//!   - modes replayed early (2027), before any content;
//!   - for a full restore, history (step 3), then newlines that scroll it
//!     just above the viewport;
//!   - the primary screen and its cursor state (steps 4 and 5);
//!   - if the program is on the alternate screen, `?47h` and the same for
//!     that screen (step 6);
//!   - terminal state without content, then the active screen's cursor
//!     again, last (step 7).
//!
//! Rows are written one at a time with Ghostty's page formatter. Joining a
//! soft-wrapped row to the next is decided here rather than by the
//! formatter's `unwrap`, which loses a row when a wrapped row is followed by
//! a blank one and would step over a continuation's leading blanks while
//! the terminal's cursor is waiting to wrap.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Writer = std.Io.Writer;
const ghostty = @import("ghostty-vt");
const modes = @import("modes.zig");

const Terminal = ghostty.Terminal;
const Screen = ghostty.Screen;
const Pin = ghostty.Pin;
const Cell = ghostty.Cell;
const Style = ghostty.Style;
const CellCountInt = ghostty.size.CellCountInt;
const PageFormatter = ghostty.formatter.PageFormatter;

pub const Mode = enum(u32) {
    /// History, then the screens and state.
    full = 0,
    /// The screens and state, without history.
    screen = 1,
};

pub const Limits = struct {
    /// The most history rows the client keeps, at its width.
    client_scrollback_rows: usize,
    /// The most bytes of history; the newest rows that fit are kept.
    history_cap: usize,
    /// The most bytes for everything but the history.
    byte_cap: usize,
};

pub const Error = Allocator.Error || error{TooLarge};

/// Build a restore of `t`, which must already be at the client's size.
/// The caller owns the result.
pub fn build(alloc: Allocator, t: *const Terminal, mode: Mode, limits: Limits) Error![]u8 {
    var aw: Writer.Allocating = .init(alloc);
    defer aw.deinit();
    var b: Builder = .{ .alloc = alloc, .aw = &aw, .t = t, .limits = limits };
    b.run(mode) catch |err| return switch (err) {
        error.WriteFailed => error.OutOfMemory,
        else => |e| e,
    };
    return aw.toOwnedSlice();
}

/// The last `history_rows` rows of history and the active screen as text
/// with SGR, wrapped rows joined, trailing blank rows dropped. No prologue,
/// no terminal state.
pub fn tail(alloc: Allocator, t: *const Terminal, history_rows: usize) Allocator.Error![]u8 {
    var aw: Writer.Allocating = .init(alloc);
    defer aw.deinit();
    writeTail(&aw.writer, t, history_rows) catch return error.OutOfMemory;
    return aw.toOwnedSlice();
}

fn writeTail(w: *Writer, t: *const Terminal, history_rows: usize) Writer.Error!void {
    const primary = t.screens.get(.primary).?;
    var rows: RowWriter = .{ .sep = "\n" };
    const total_history = historyRows(primary);
    const n = @min(total_history, history_rows);
    if (n > 0) {
        const first = historyPin(primary, total_history - n);
        const last = historyPin(primary, total_history - 1);
        var it = first.rowIterator(.right_down, last);
        while (it.next()) |row| try rows.write(w, row);
    }
    const active = t.screens.active;
    // History and an alternate screen are not one stream of lines.
    if (active != primary and rows.prev != null) {
        try w.writeAll(rows.sep);
        rows.prev = null;
    }
    if (lastUsedRow(active)) |last_y| {
        const first = activePin(active, 0);
        const last = activePin(active, last_y);
        var it = first.rowIterator(.right_down, last);
        while (it.next()) |row| try rows.write(w, row);
    }
    if (rows.prev != null) try w.writeAll("\n");
}

const Builder = struct {
    alloc: Allocator,
    aw: *Writer.Allocating,
    t: *const Terminal,
    limits: Limits,
    /// Bytes of history written, which `byte_cap` does not count.
    history_len: usize = 0,

    fn out(self: *Builder) *Writer {
        return &self.aw.writer;
    }

    /// Fail once the restore passes its byte limit. Checked after each row
    /// and each region, so the output overshoots by at most one row.
    fn check(self: *Builder) error{TooLarge}!void {
        if (self.aw.written().len - self.history_len > self.limits.byte_cap) return error.TooLarge;
    }

    fn run(self: *Builder, mode: Mode) (Error || Writer.Error)!void {
        const t = self.t;
        try writeModes(self.out(), t, .early);
        if (mode == .full) {
            const start = self.aw.written().len;
            try self.writeHistory();
            self.history_len = self.aw.written().len - start;
        }

        const primary = t.screens.get(.primary).?;
        try self.writeScreen(primary);
        if (t.screens.active_key == .alternate) {
            // Mode 47 whatever mode the program used: it saves no cursor,
            // so the primary screen's saved cursor written above survives,
            // and it does not clear, so the restore clears.
            try self.out().writeAll("\x1b[?47h");
            try self.writeScreen(t.screens.get(.alternate).?);
        }
        try self.writeState();
        try self.check();
    }

    /// Step 3: the newest history rows that fit the client's scrollback and
    /// the history cap, then newlines that scroll them just above the
    /// viewport so that clearing the screen next does not erase them.
    fn writeHistory(self: *Builder) (Error || Writer.Error)!void {
        const screen = self.t.screens.get(.primary).?;
        const total = historyRows(screen);
        const n = @min(total, self.limits.client_scrollback_rows);
        if (n == 0) return;

        // Measure each row (and the line break before it) to find the
        // oldest row from which the rest fits the cap.
        const content = try self.alloc.alloc(usize, n);
        defer self.alloc.free(content);
        const sep = try self.alloc.alloc(usize, n);
        defer self.alloc.free(sep);
        {
            var rows: RowWriter = .{ .sep = "\r\n" };
            var it = historyPin(screen, total - n).rowIterator(.right_down, historyPin(screen, total - 1));
            var i: usize = 0;
            while (it.next()) |row| : (i += 1) {
                var counter: Writer.Discarding = .init(&.{});
                sep[i] = if (rows.prev) |prev| (if (joins(prev, row)) 0 else rows.sep.len) else 0;
                try writeRowCells(&counter.writer, row);
                content[i] = @intCast(counter.count);
                rows.prev = row;
            }
            std.debug.assert(i == n);
        }
        var start = n;
        var total_bytes: usize = 0;
        while (start > 0) {
            const i = start - 1;
            const add = content[i] + if (i + 1 < n) sep[i + 1] else 0;
            if (total_bytes + add > self.limits.history_cap) break;
            total_bytes += add;
            start = i;
        }
        if (start == n) return;

        var rows: RowWriter = .{ .sep = "\r\n" };
        var it = historyPin(screen, total - n + start).rowIterator(.right_down, historyPin(screen, total - 1));
        while (it.next()) |row| try rows.write(self.out(), row);
        try self.out().splatBytesAll("\r\n", self.t.rows);
    }

    /// Steps 4 and 5 (or 6): clear, draw the active area, then the cursor
    /// state, while this screen is the client's active one.
    fn writeScreen(self: *Builder, screen: *const Screen) (Error || Writer.Error)!void {
        const w = self.out();
        // The previous screen's pen and character sets must not reach the
        // clear or the cells: `?47h` keeps them.
        try writeCleanSlate(w);
        try w.writeAll("\x1b[H\x1b[2J");
        if (lastUsedRow(screen)) |last| {
            var rows: RowWriter = .{ .sep = "\r\n" };
            var it = activePin(screen, 0).rowIterator(.right_down, activePin(screen, last));
            while (it.next()) |row| {
                try rows.write(w, row);
                try self.check();
            }
        }

        try writeSavedCursor(w, screen);
        try writeCleanSlate(w);
        // No margins or origin mode are set yet, so pending wrap can only
        // be rebuilt at the right edge of the screen here. The active
        // screen's cursor is written again, exactly, in step 7.
        try writeCursor(w, screen, .{}, true);
        try writePen(w, screen);
        try writeKittyStack(w, screen);
        try self.check();
    }

    /// Step 7: terminal state without content, then the active screen's
    /// cursor again, because scroll regions, tab stops and origin mode move
    /// it.
    fn writeState(self: *Builder) Writer.Error!void {
        const w = self.out();
        const t = self.t;

        // Before the modes, so that a replayed `?12h` (cursor blinking)
        // has the last word, as it had in the model.
        try writeCursorStyle(w, t);

        // Before the modes too: CHA counts from the left margin in origin
        // mode.
        if (!defaultTabstops(t)) {
            try w.writeAll("\x1b[3g");
            for (0..t.cols) |col| {
                if (t.tabstops.get(col)) try w.print("\x1b[{d}G\x1bH", .{col + 1});
            }
        }

        try writeModes(w, t, .yes);

        const region = t.scrolling_region;
        if (region.top != 0 or region.bottom != t.rows - 1) {
            try w.print("\x1b[{d};{d}r", .{ region.top + 1, region.bottom + 1 });
        }
        if (region.left != 0 or region.right != t.cols - 1) {
            try w.print("\x1b[{d};{d}s", .{ region.left + 1, region.right + 1 });
        }

        if (t.flags.modify_other_keys_2) try w.writeAll("\x1b[>4;2m");

        // Only what the program changed: the client's own palette and
        // colors stay for the rest.
        const palette = &t.colors.palette;
        for (0..palette.current.len) |i| {
            if (!palette.mask.isSet(i)) continue;
            const rgb = palette.current[i];
            try w.print("\x1b]4;{d};rgb:{x:0>2}/{x:0>2}/{x:0>2}\x1b\\", .{ i, rgb.r, rgb.g, rgb.b });
        }
        for ([_]struct { u8, ghostty.color.DynamicRGB }{
            .{ 10, t.colors.foreground },
            .{ 11, t.colors.background },
            .{ 12, t.colors.cursor },
        }) |pair| {
            const rgb = pair[1].override orelse continue;
            try w.print("\x1b]{d};rgb:{x:0>2}/{x:0>2}/{x:0>2}\x1b\\", .{ pair[0], rgb.r, rgb.g, rgb.b });
        }

        if (t.getTitle()) |title| if (title.len > 0) {
            try w.writeAll("\x1b]2;");
            try writeOscText(w, title);
            try w.writeAll("\x1b\\");
        };
        if (t.getPwd()) |pwd| if (pwd.len > 0) {
            try w.writeAll("\x1b]7;");
            try writeOscText(w, pwd);
            try w.writeAll("\x1b\\");
        };

        const active = t.screens.active;
        const origin: Origin = if (t.modes.get(.origin))
            .{ .x = region.left, .y = region.top }
        else
            .{};
        try writeCleanSlate(w);
        try writeCursor(w, active, origin, false);
        try writePen(w, active);

        // Last of all: in insert mode the edge cell printed above would have
        // pushed the row right.
        if (t.modes.get(.insert)) try w.writeAll("\x1b[4h");
    }
};

/// Writes rows one after another, joining a soft-wrapped row to the next
/// when the terminal will wrap there by itself.
const RowWriter = struct {
    sep: []const u8,
    prev: ?Pin = null,

    fn write(self: *RowWriter, w: *Writer, row: Pin) Writer.Error!void {
        if (self.prev) |prev| {
            if (!joins(prev, row)) try w.writeAll(self.sep);
        }
        try writeRowCells(w, row);
        self.prev = row;
    }
};

/// One row's cells, with styles and hyperlinks, ending with both closed.
fn writeRowCells(w: *Writer, row: Pin) Writer.Error!void {
    var f: PageFormatter = .init(row.node.page(), .vt);
    f.start_y = row.y;
    f.end_y = row.y;
    try f.format(w);
}

/// Whether printing `row` straight after `prev` makes the terminal wrap
/// onto it as Ghostty did: `prev` is soft-wrapped and ends with a printed
/// cell at the edge, and `row` starts with a printed cell (the formatter
/// steps over leading blanks, which would not wrap).
fn joins(prev: Pin, row: Pin) bool {
    const prev_page = prev.node.page();
    const prev_row = prev_page.getRow(prev.y);
    if (!prev_row.wrap) return false;
    const page = row.node.page();
    const this_row = page.getRow(row.y);
    if (!this_row.wrap_continuation) return false;
    return endsAtEdge(prev_page.getCells(prev_row)) and printsAsText(page.getCells(this_row)[0]);
}

/// Whether the formatter writes this cell as a character (rather than
/// stepping over it or erasing it with its background).
fn printsAsText(cell: Cell) bool {
    return switch (cell.content_tag) {
        .codepoint, .codepoint_grapheme => switch (cell.wide) {
            .narrow, .wide => cell.hasText() or cell.hasStyling(),
            .spacer_head, .spacer_tail => false,
        },
        .bg_color_palette, .bg_color_rgb => false,
    };
}

/// Whether writing these cells leaves the cursor at the right edge: waiting
/// to wrap, or before a wide character that did not fit.
fn endsAtEdge(cells: []const Cell) bool {
    const last = cells[cells.len - 1];
    return switch (last.wide) {
        .narrow, .wide => printsAsText(last),
        .spacer_tail => true,
        .spacer_head => cells.len >= 2 and
            (printsAsText(cells[cells.len - 2]) or cells[cells.len - 2].wide == .spacer_tail),
    };
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

/// The last row of the active area that the formatter writes anything for.
fn lastUsedRow(screen: *const Screen) ?usize {
    var y: usize = screen.pages.rows;
    while (y > 0) {
        y -= 1;
        const pin = activePin(screen, y);
        const page = pin.node.page();
        for (page.getCells(page.getRow(pin.y))) |cell| {
            if (!cell.isEmpty() or cell.hasStyling()) return y;
        }
    }
    return null;
}

/// Where CUP counts from: the screen's top-left, or the scroll region's in
/// origin mode.
const Origin = struct {
    x: CellCountInt = 0,
    y: CellCountInt = 0,
};

/// Step 5.2: reset the pen, character sets, protection and hyperlink, so
/// that the next print shows what the cell holds.
fn writeCleanSlate(w: *Writer) Writer.Error!void {
    try w.writeAll("\x1b[0m\x1b(B\x0f\x1b[0\"q\x1b]8;;\x1b\\");
}

/// Step 5.1: the saved cursor, set while this screen is active and saved
/// with DECSC.
fn writeSavedCursor(w: *Writer, screen: *const Screen) Writer.Error!void {
    const saved = screen.saved_cursor orelse return;
    const cols = screen.pages.cols;
    const x = @min(saved.x, cols - 1);
    const y = @min(saved.y, screen.pages.rows - 1);

    try writeCleanSlate(w);
    // Setting origin mode homes the cursor, so it comes first. No scroll
    // region is set yet, so positions are absolute either way.
    if (saved.origin) try w.writeAll("\x1b[?6h");
    if (saved.pending_wrap and x == cols - 1) {
        const row = activePin(screen, y);
        const start = edgeStart(row, x);
        try writeCup(w, y, start, .{});
        try writeEdgeCell(w, row, start);
    } else {
        try writeCup(w, y, x, .{});
    }
    try w.print("{f}", .{saved.style.formatterVt()});
    if (saved.protected) try w.writeAll("\x1b[1\"q");
    try writeCharsets(w, &saved.charset);
    try w.writeAll("\x1b7");
    if (saved.origin) try w.writeAll("\x1b[?6l");
}

/// Step 5.3: the live cursor's position and pending wrap. Pending wrap is
/// rebuilt by printing the cell at the edge in its own style, never with
/// DECSC and DECRC, which would overwrite the saved cursor. With
/// `edge_only`, only a cursor at the screen's right edge gets it.
fn writeCursor(w: *Writer, screen: *const Screen, origin: Origin, edge_only: bool) Writer.Error!void {
    const cursor = &screen.cursor;
    if (cursor.pending_wrap and (!edge_only or cursor.x == screen.pages.cols - 1)) {
        const row = activePin(screen, cursor.y);
        const start = edgeStart(row, cursor.x);
        try writeCup(w, cursor.y, start, origin);
        try writeEdgeCell(w, row, start);
    } else {
        try writeCup(w, cursor.y, cursor.x, origin);
    }
}

fn writeCup(w: *Writer, y: CellCountInt, x: CellCountInt, origin: Origin) Writer.Error!void {
    try w.print("\x1b[{d};{d}H", .{ (y -| origin.y) + 1, (x -| origin.x) + 1 });
}

/// The column to print from to leave the cursor waiting to wrap at `x`:
/// the start of a wide character whose second half is at `x`.
fn edgeStart(row: Pin, x: CellCountInt) CellCountInt {
    const page = row.node.page();
    const cell = page.getRowAndCell(x, row.y).cell;
    return if (cell.wide == .spacer_tail and x > 0) x - 1 else x;
}

/// Print one cell in its own style, hyperlink and protection, then reset
/// all three. A cell with no text is printed as a space.
fn writeEdgeCell(w: *Writer, row: Pin, x: CellCountInt) Writer.Error!void {
    const page = row.node.page();
    const cell = page.getRowAndCell(x, row.y).cell;
    const style: Style = switch (cell.content_tag) {
        .codepoint, .codepoint_grapheme => if (cell.hasStyling())
            page.styles.get(page.memory, cell.style_id).*
        else
            .{},
        .bg_color_palette => .{ .bg_color = .{ .palette = cell.content.color_palette.data } },
        .bg_color_rgb => .{ .bg_color = .{ .rgb = .{
            .r = cell.content.color_rgb.r,
            .g = cell.content.color_rgb.g,
            .b = cell.content.color_rgb.b,
        } } },
    };
    try w.print("{f}", .{style.formatterVt()});
    if (cell.protected) try w.writeAll("\x1b[1\"q");
    var linked = false;
    if (cell.hyperlink) if (page.lookupHyperlink(cell)) |id| {
        const link = page.hyperlink_set.get(page.memory, id);
        const uri = link.uri.slice(page.memory);
        switch (link.id) {
            .explicit => |explicit| try w.print("\x1b]8;id={s};{s}\x1b\\", .{ explicit.slice(page.memory), uri }),
            .implicit => try w.print("\x1b]8;;{s}\x1b\\", .{uri}),
        }
        linked = true;
    };
    if (cell.hasText()) {
        try w.printUnicodeCodepoint(cell.codepoint());
        if (cell.content_tag == .codepoint_grapheme) {
            if (page.lookupGrapheme(cell)) |cps| for (cps) |cp| try w.printUnicodeCodepoint(cp);
        }
    } else {
        try w.writeByte(' ');
    }
    if (linked) try w.writeAll("\x1b]8;;\x1b\\");
    if (cell.protected) try w.writeAll("\x1b[0\"q");
    try w.writeAll("\x1b[0m");
}

/// Step 5.4: the live cursor's pen, hyperlink, protection and character
/// sets, after the clean slate.
fn writePen(w: *Writer, screen: *const Screen) Writer.Error!void {
    const cursor = &screen.cursor;
    try w.print("{f}", .{cursor.style.formatterVt()});
    if (cursor.hyperlink) |link| switch (link.id) {
        .explicit => |id| try w.print("\x1b]8;id={s};{s}\x1b\\", .{ id, link.uri }),
        .implicit => try w.print("\x1b]8;;{s}\x1b\\", .{link.uri}),
    };
    if (cursor.protected) try w.writeAll("\x1b[1\"q");
    try writeCharsets(w, &screen.charset);
}

/// All four designations and both invocations, so that whatever an earlier
/// step designated is replaced.
fn writeCharsets(w: *Writer, state: *const Screen.CharsetState) Writer.Error!void {
    for ([_]struct { ghostty.CharsetSlot, u8 }{
        .{ .G0, '(' }, .{ .G1, ')' }, .{ .G2, '*' }, .{ .G3, '+' },
    }) |pair| {
        const final: u8 = switch (state.charsets.get(pair[0])) {
            .utf8, .ascii => 'B',
            .british => 'A',
            .dec_special => '0',
        };
        try w.print("\x1b{c}{c}", .{ pair[1], final });
    }
    try w.writeAll(switch (state.gl) {
        .G0 => "\x0f", // SI
        .G1 => "\x0e", // SO
        .G2 => "\x1bn", // LS2
        .G3 => "\x1bo", // LS3
    });
    try w.writeAll(switch (state.gr) {
        .G0, .G2 => "\x1b}", // LS2R; GR cannot be G0
        .G1 => "\x1b~", // LS1R
        .G3 => "\x1b|", // LS3R
    });
}

/// Step 5.5: Ghostty keeps 8 entries in a ring and does not record which
/// zeros were pushed, so the whole ring is pushed, oldest first, ending
/// with the current entry. Each push displaces the client's empty base
/// entry in turn. Nothing is written when every entry is zero.
fn writeKittyStack(w: *Writer, screen: *const Screen) Writer.Error!void {
    const stack = &screen.kitty_keyboard;
    for (stack.flags) |f| {
        if (f.int() != 0) break;
    } else return;
    for (1..stack.flags.len + 1) |i| {
        const f = stack.flags[(@as(usize, stack.idx) + i) % stack.flags.len];
        try w.print("\x1b[>{d}u", .{f.int()});
    }
}

/// Modes with the given replay mark, where they differ from Ghostty's
/// default. Insert mode is left to the caller, and the mouse modes are
/// written from the effective mouse state.
fn writeModes(w: *Writer, t: *const Terminal, which: modes.Replay) Writer.Error!void {
    for (modes.table) |e| {
        if (e.replay != which) continue;
        switch (e.mode) {
            .insert,
            .mouse_event_x10,
            .mouse_event_normal,
            .mouse_event_button,
            .mouse_event_any,
            .mouse_format_utf8,
            .mouse_format_sgr,
            .mouse_format_urxvt,
            .mouse_format_sgr_pixels,
            => continue,
            else => {},
        }
        const current = t.modes.get(e.mode);
        if (current != e.default()) try writeMode(w, e.mode, current);
    }
    if (which != .yes) return;

    // Ghostty keeps each mouse mode's bit but acts on the last one set
    // (and on none after any of them is reset), so the restore sets just
    // the one it acts on.
    switch (t.flags.mouse_event) {
        .none => {},
        .x10 => try writeMode(w, .mouse_event_x10, true),
        .normal => try writeMode(w, .mouse_event_normal, true),
        .button => try writeMode(w, .mouse_event_button, true),
        .any => try writeMode(w, .mouse_event_any, true),
    }
    switch (t.flags.mouse_format) {
        .x10 => {},
        .utf8 => try writeMode(w, .mouse_format_utf8, true),
        .sgr => try writeMode(w, .mouse_format_sgr, true),
        .urxvt => try writeMode(w, .mouse_format_urxvt, true),
        .sgr_pixels => try writeMode(w, .mouse_format_sgr_pixels, true),
    }
}

fn writeMode(w: *Writer, mode: ghostty.Mode, value: bool) Writer.Error!void {
    const tag: ghostty.modes.ModeTag = .fromMode(mode);
    try w.print("\x1b[{s}{d}{c}", .{ if (tag.ansi) "" else "?", tag.value, @as(u8, if (value) 'h' else 'l') });
}

/// DECSCUSR. `CSI 0 SP q` gives the client its own default back when the
/// program never chose a style.
fn writeCursorStyle(w: *Writer, t: *const Terminal) Writer.Error!void {
    if (t.cursor.is_default) return w.writeAll("\x1b[0 q");
    const blink = t.modes.get(.cursor_blinking);
    const n: u8 = switch (t.screens.active.cursor.cursor_style) {
        .block, .block_hollow => if (blink) 1 else 2,
        .underline => if (blink) 3 else 4,
        .bar => if (blink) 5 else 6,
    };
    try w.print("\x1b[{d} q", .{n});
}

/// Whether the tab stops are Ghostty's defaults, every 8 columns, which the
/// prologue sets.
fn defaultTabstops(t: *const Terminal) bool {
    for (0..t.cols) |col| {
        const expected = col > 0 and col % 8 == 0 and col < t.cols - 1;
        if (t.tabstops.get(col) != expected) return false;
    }
    return true;
}

/// Text inside an OSC, without the controls that would end or corrupt it.
fn writeOscText(w: *Writer, text: []const u8) Writer.Error!void {
    for (text) |c| {
        if (c < 0x20 or c == 0x7f) continue;
        try w.writeByte(c);
    }
}
