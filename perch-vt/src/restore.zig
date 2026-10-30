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
    var joiner: Joiner = undefined;
    const cluster = t.modes.get(.grapheme_cluster);
    if (cluster) try joiner.init(alloc);
    defer if (cluster) joiner.deinit(alloc);
    var b: Builder = .{ .alloc = alloc, .aw = &aw, .t = t, .limits = limits, .joiner = if (cluster) &joiner else null };
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
        while (it.next()) |row| try rows.write(w, row, null);
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
        while (it.next()) |row| try rows.write(w, row, null);
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
    /// With grapheme clustering (2027), finds the cells that would cluster.
    joiner: ?*Joiner = null,

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
        // Before anything else, since DECSTR resets what it touches.
        if (savedCarrier(t)) try self.out().writeAll("\x1b[1\"q\x1b[?47h\x1b7\x1b[?47l\x1b[!p");
        try writeDefaultCursorStyle(self.out(), t);
        if (mode == .full) {
            const start = self.aw.written().len;
            try self.writeHistory();
            self.history_len = self.aw.written().len - start;
        }

        const primary = t.screens.get(.primary).?;
        try self.writeScreen(primary);
        if (t.screens.active_key == .alternate) {
            // The primary screen's own cursor style, which 1049 does not
            // copy back on leaving (see `writeDefaultCursorStyle` for the
            // default). Step 7 writes the active screen's.
            const style = primary.cursor.cursor_style;
            if (!t.cursor.is_default and style != t.cursor.default_style) {
                try self.out().print("\x1b[{d} q", .{steadyStyle(style)});
            }
            // Mode 47 whatever mode the program used: it saves no cursor,
            // so the primary screen's saved cursor written above survives,
            // and it does not clear, so the restore clears.
            try self.out().writeAll("\x1b[?47h");
            // Protection carried to the primary screen through this one
            // (see `writeProtect`) set its mode; only DECSTR turns that
            // off again. What else it resets is written after this.
            const alt = t.screens.get(.alternate).?;
            const saved_protected = if (primary.saved_cursor) |sc| sc.protected else false;
            if (primary.protected_mode == .off and (primary.cursor.protected or saved_protected) and
                alt.protected_mode == .off) try self.out().writeAll("\x1b[!p");
            // 47 copied the primary screen's style; this one's, if it has
            // the default since.
            if (t.cursor.is_default and alt.cursor.cursor_style != primary.cursor.cursor_style and
                alt.cursor.cursor_style == t.cursor.default_style) try self.out().writeAll("\x1b[0 q");
            try self.writeScreen(alt);
        }
        try self.writeState();
        try self.check();
    }

    /// Step 3: the newest history rows that fit the client's scrollback and
    /// the history cap, then newlines that scroll them just above the
    /// viewport so that clearing the screen next does not erase them.
    fn writeHistory(self: *Builder) (Error || Writer.Error)!void {
        const screen = self.t.screens.get(.primary).?;
        const kept = try historyKept(self.alloc, self.t, self.limits);
        if (kept == 0) return;
        const total = historyRows(screen);

        var rows: RowWriter = .{ .sep = "\r\n", .joiner = self.joiner, .rows = self.t.rows };
        var it = historyPin(screen, total - kept).rowIterator(.right_down, historyPin(screen, total - 1));
        var next = it.next();
        while (next) |row| {
            next = it.next();
            try rows.write(self.out(), row, next);
        }
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
            var rows: RowWriter = .{ .sep = "\r\n", .joiner = self.joiner, .rows = self.t.rows };
            var it = activePin(screen, 0).rowIterator(.right_down, activePin(screen, last));
            var next = it.next();
            while (next) |row| {
                next = it.next();
                try rows.write(w, row, next);
                try self.check();
            }
        }

        try writeSavedCursor(w, self.t, screen);
        try writeCleanSlate(w);
        // No margins or origin mode are set yet, so pending wrap can only
        // be rebuilt at the right edge of the screen here. The active
        // screen's cursor is written again, exactly, in step 7.
        try writeCursor(w, screen, .{}, true);
        const c = &screen.cursor;
        if (screen == self.t.screens.get(.primary).? and self.t.screens.active_key == .alternate and
            c.pending_wrap and c.x != screen.pages.cols - 1 and c.x > 0)
        {
            // Hidden while the alternate screen is active, this cursor is
            // not written again in step 7, so its pending wrap away from
            // the edge is made here, at a right margin set there for it
            // (which homes the cursor) and dropped again (which does not).
            const row = activePin(screen, c.y);
            const start = edgeStart(row, c.x);
            try w.print("\x1b[?69h\x1b[1;{d}s", .{c.x + 1});
            try writeCup(w, c.y, start, .{});
            try writeEdgeCell(w, screen, row, start);
            try w.writeAll("\x1b[?69l");
        }
        try writePen(w, self.t, screen);
        if (carriedProtection(self.t, screen) or carriedSaved(self.t, screen)) try self.carryProtection();
        try writeKittyStack(w, screen);
        try self.check();
    }

    /// The alternate screen's cursor protected with its mode off (see
    /// `carriedProtection`): it can only come from the primary screen's
    /// cursor, which 47 copies. So the primary cursor is set as the model
    /// has it, protected in its own mode, and copied back, then this
    /// cursor's own position and pen. Also run after carrying a saved
    /// cursor's protection (`carriedSaved`), which left the primary
    /// cursor as this one.
    fn carryProtection(self: *Builder) Writer.Error!void {
        const w = self.out();
        const primary = self.t.screens.get(.primary).?;
        const alt = self.t.screens.get(.alternate).?;
        try writeCleanSlate(w);
        if (primary.protected_mode != .off) try self.copyPrimaryCursor();
        if (!alt.cursor.protected) {
            try w.writeAll("\x1b[0\"q");
        } else if (primary.protected_mode == .off or !primary.cursor.protected) {
            // From this screen's saved cursor, protected as written above
            // (DECRC sets the cursor's protection, not the mode), as the
            // model's could be. It also restores origin mode, which is
            // replayed in step 7.
            try w.writeAll("\x1b8\x1b[?6l");
        }
        try writeCursor(w, alt, .{}, true);
        try writePen(w, self.t, alt);
    }

    /// The primary cursor as the model has it, protected in the primary
    /// screen's own mode, and copied to this one with 47.
    fn copyPrimaryCursor(self: *Builder) Writer.Error!void {
        const w = self.out();
        const primary = self.t.screens.get(.primary).?;
        const alt = self.t.screens.get(.alternate).?;
        // The primary cursor's place, waiting to wrap by printing this
        // screen's own cell there: at the edge, or away from it at a right
        // margin set there for it (which homes the cursor) and dropped
        // again (which does not) before the copy.
        const c = &primary.cursor;
        const interior = c.pending_wrap and c.x != alt.pages.cols - 1 and c.x > 0;
        if (c.pending_wrap and (c.x == alt.pages.cols - 1 or interior)) {
            const row = activePin(alt, c.y);
            const start = edgeStart(row, c.x);
            if (interior) try w.print("\x1b[?69h\x1b[1;{d}s", .{c.x + 1});
            try writeCup(w, c.y, start, .{});
            try writeCell(w, row, start, false);
            if (interior) try w.writeAll("\x1b[?69l");
        } else try writeCup(w, c.y, c.x, .{});
        try w.print("{f}", .{c.style.formatterVt()});
        try w.writeAll("\x1b[?47l");
        try writePen(w, self.t, primary);
        try w.writeAll("\x1b[?47h\x1b[0m");
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
        if (carriedProtection(t, active)) {
            // The clean slate without unprotecting what was carried over.
            try w.writeAll("\x1b[0m\x1b(B\x0f\x1b]8;;\x1b\\");
        } else try writeCleanSlate(w);
        try writeCursor(w, active, origin, false);
        try writePen(w, t, active);

        // Last of all: in insert mode the edge cell printed above would have
        // pushed the row right, and without wraparound it would have been
        // printed over itself, losing a combining mark.
        if (t.modes.get(.insert)) try w.writeAll("\x1b[4h");
        if (!t.modes.get(.wraparound)) try w.writeAll("\x1b[?7l");
    }
};

/// How many of the newest history rows a full restore of `t` (already at
/// the client's size) sends: as many as fit the client's scrollback and
/// the history cap, counting the line breaks and wrap primers between them.
pub fn historyKept(alloc: Allocator, t: *const Terminal, limits: Limits) Allocator.Error!usize {
    const screen = t.screens.get(.primary).?;
    const total = historyRows(screen);
    const n = @min(total, limits.client_scrollback_rows);
    if (n == 0) return 0;

    // Each row's bytes written as the first row, and as written after the
    // row before (with a line break, a wrap primer or nothing between, and
    // that row's cells fixed up after a wrap). The second is not always the
    // larger: a row can be written whole after a wrap that it would be
    // written in runs without.
    const first = try alloc.alloc(usize, n);
    defer alloc.free(first);
    const after = try alloc.alloc(usize, n);
    defer alloc.free(after);
    var joiner: Joiner = undefined;
    const cluster = t.modes.get(.grapheme_cluster);
    if (cluster) try joiner.init(alloc);
    defer if (cluster) joiner.deinit(alloc);
    var rows: RowWriter = .{ .sep = "\r\n", .joiner = if (cluster) &joiner else null, .rows = t.rows };
    var it = historyPin(screen, total - n).rowIterator(.right_down, historyPin(screen, total - 1));
    var i: usize = 0;
    var next = it.next();
    while (next) |row| : (i += 1) {
        next = it.next();
        // The row as the first one written, and as written after the one
        // before (whose cells fixed up after a wrap count here).
        var alone: RowWriter = .{ .sep = rows.sep, .joiner = rows.joiner, .rows = rows.rows };
        var own: Writer.Discarding = .init(&.{});
        alone.write(&own.writer, row, next) catch return error.OutOfMemory;
        var all: Writer.Discarding = .init(&.{});
        rows.write(&all.writer, row, next) catch return error.OutOfMemory;
        first[i] = @intCast(own.count);
        after[i] = @intCast(all.count);
    }
    std.debug.assert(i == n);

    // The oldest row from which the rest fits the cap: that row written
    // first, and each after it as written after the one before.
    var start = n;
    var rest: usize = 0;
    while (start > 0) {
        const j = start - 1;
        if (first[j] + rest > limits.history_cap) break;
        rest += after[j];
        start = j;
    }
    return n - start;
}

/// Writes rows one after another, joining a soft-wrapped row to the next
/// when the terminal will wrap there by itself.
const RowWriter = struct {
    sep: []const u8,
    /// Set when grapheme clustering (2027) is on.
    joiner: ?*Joiner = null,
    /// The screen's height.
    rows: usize = 0,
    prev: ?Pin = null,
    /// A row whose takers (see `Joiner`) are left to write after the wrap
    /// onto the next, which needs the cursor waiting at the edge; and
    /// whether that row was wrapped onto from a spacer head.
    pending: ?struct { Pin, bool } = null,

    /// `next` is the row that will be written after this one, if any.
    fn write(self: *RowWriter, w: *Writer, row: Pin, next: ?Pin) Writer.Error!void {
        const from_head = if (self.prev) |prev| joins(prev, row) and endsBeforeWideCharacter(prev) else false;
        const wraps = if (next) |n| joins(row, n) else false;
        const defer_taken = wraps and !endsBeforeWideCharacter(row);
        var split = false;
        var first = false;
        if (self.joiner) |j| if (try j.splits(row)) {
            first = from_head and try j.takesNext(row, 0);
            // Fixing up a row from the one below or above needs both on
            // the screen.
            split = self.rows > 1 or !(defer_taken or first);
            first = split and first;
        };

        if (self.prev) |prev| {
            if (!joins(prev, row)) {
                try w.writeAll(self.sep);
            } else if (from_head) {
                // Only the wide character itself wraps here.
                if (startsWithBackground(row) or first) try writeWrapPrimer(w, row);
                // A taker: the next cell must be written while it is
                // blank. (This clears the spacer head too, which
                // `Joiner.writeTaken` makes again.)
                if (first) try w.writeAll("\x1b[2X");
            } else if (self.joiner != null or startsWithBackground(row)) {
                // A space with the default pen makes the wrap: it joins no
                // grapheme cluster (with 2027 a printed character can
                // attach to the cell waiting at the edge instead of
                // wrapping), and it scrolls in a row without a background.
                // Then step back, so the row is written from its start,
                // where nothing is left of it to join.
                try w.writeAll("\x1b[0m \x08");
            }
        }
        if (self.pending) |pending| {
            // Back up to the row before, now that the wrap is made, and
            // down again to this one's start.
            try w.writeAll("\x1b[A");
            try self.joiner.?.writeTaken(w, pending[0], pending[1]);
            try w.writeAll("\x1b[B\r");
            self.pending = null;
        }
        if (!split) {
            try writeRowCells(w, row);
        } else {
            const j = self.joiner.?;
            try j.writeRest(w, row);
            if (defer_taken) {
                self.pending = .{ row, from_head };
            } else {
                try j.writeTaken(w, row, from_head);
                // Back before the spacer head, where the wide character
                // that follows wraps by itself.
                if (wraps) try w.print("\x1b[{d}G", .{row.node.page().size.cols});
            }
        }
        self.prev = row;
    }
};

/// With grapheme clustering (2027), a terminal adds a printed character to
/// the cluster in the cell to its left when they do not break, wherever the
/// cursor came from. A program can still leave such cells apart (printing
/// the right one first), and written in order they would join, shifting the
/// rest of the row. Such a row is written without the cells that would take
/// in the next ("takers"), each run after the first placed with CHA, then
/// the takers from right to left, each while the cell to its right is in
/// place and the one to its left (if a taker) still blank.
///
/// Whether two cells join is asked of a scratch terminal, so that it is
/// Ghostty's own rule.
const Joiner = struct {
    t: Terminal,

    fn init(self: *Joiner, alloc: Allocator) Allocator.Error!void {
        self.t = Terminal.init(ghostty.TinyIo.init.io(), alloc, .{ .cols = 64, .rows = 1 }) catch
            return error.OutOfMemory;
        self.t.modes.set(.grapheme_cluster, true);
    }

    fn deinit(self: *Joiner, alloc: Allocator) void {
        self.t.deinit(alloc);
    }

    /// Whether the row has a taker.
    fn splits(self: *Joiner, row: Pin) Writer.Error!bool {
        const cols = row.node.page().size.cols;
        var x: CellCountInt = 0;
        while (x < cols) : (x += 1) {
            if (try self.takesNext(row, x)) return true;
        }
        return false;
    }

    /// Whether the cell at `x` (a narrow or wide character) takes in the
    /// character in the cell after it when written just before.
    fn takesNext(self: *Joiner, row: Pin, x: CellCountInt) Writer.Error!bool {
        const page = row.node.page();
        const cells = page.getCells(page.getRow(row.y));
        const left = &cells[x];
        if (!left.hasText()) return false;
        const after = x + @as(CellCountInt, if (left.wide == .wide) 2 else 1);
        switch (left.wide) {
            .narrow, .wide => {},
            .spacer_head, .spacer_tail => return false,
        }
        if (after >= cells.len) return false;
        const right = &cells[after];
        // Ghostty clusters only codepoints above U+00FF.
        if (!right.hasText() or right.codepoint() <= 0xff) return false;

        const t = &self.t;
        t.carriageReturn();
        t.eraseLine(.complete, false);
        self.print(left.codepoint()) catch return error.WriteFailed;
        if (left.content_tag == .codepoint_grapheme) {
            if (page.lookupGrapheme(left)) |cps| for (cps) |cp| {
                self.print(cp) catch return error.WriteFailed;
            };
        }
        const start = t.screens.active.cursor.x;
        self.print(right.codepoint()) catch return error.WriteFailed;
        const pin = t.screens.active.pages.pin(.{ .active = .{} }).?;
        const scratch = pin.node.page();
        return !scratch.getCells(scratch.getRow(pin.y))[start].hasText();
    }

    fn print(self: *Joiner, cp: u21) !void {
        // Past the scratch row's edge, nothing more can join.
        if (self.t.screens.active.cursor.x + 2 >= self.t.cols) return;
        try self.t.print(cp);
    }

    /// The row's cells without its takers, in runs.
    fn writeRest(self: *Joiner, w: *Writer, row: Pin) Writer.Error!void {
        const cols = row.node.page().size.cols;
        var start: CellCountInt = 0;
        var x: CellCountInt = 0;
        while (x < cols) : (x += 1) {
            if (!try self.takesNext(row, x)) continue;
            if (x > start) try writeRun(w, row, start, x - 1);
            start = x + @as(CellCountInt, if (cellAt(row, x).wide == .wide) 2 else 1);
        }
        if (start < cols) try writeRun(w, row, start, cols - 1);
    }

    /// The takers, right to left, from anywhere on the row. The formatter
    /// writes no protection, and neither does this. A wide taker at the
    /// start of a row wrapped onto from a spacer head (`from_head`) is
    /// printed at the edge of the row above, where it wraps again and
    /// makes the spacer head.
    fn writeTaken(self: *Joiner, w: *Writer, row: Pin, from_head: bool) Writer.Error!void {
        const cols = row.node.page().size.cols;
        var x = cols;
        while (x > 0) {
            x -= 1;
            if (!try self.takesNext(row, x)) continue;
            if (x == 0 and from_head) {
                try w.print("\x1b[A\x1b[{d}G", .{cols});
            } else {
                try w.print("\x1b[{d}G", .{x + 1});
            }
            try writeCell(w, row, x, false);
        }
    }
};

fn cellAt(row: Pin, x: CellCountInt) *const Cell {
    return row.node.page().getRowAndCell(x, row.y).cell;
}

/// Columns `start` to `end` of a row, from `start` (where the cursor is
/// for the row's first run).
fn writeRun(w: *Writer, row: Pin, start: CellCountInt, end: CellCountInt) Writer.Error!void {
    if (start > 0) try w.print("\x1b[{d}G", .{start + 1});
    var f: PageFormatter = .init(row.node.page(), .vt);
    f.start_y = row.y;
    f.end_y = row.y;
    f.start_x = start;
    f.end_x = end;
    try f.format(w);
}

/// Whether the row ends where a wide character did not fit, leaving the
/// cursor before the edge rather than waiting to wrap.
fn endsBeforeWideCharacter(row: Pin) bool {
    const page = row.node.page();
    const cells = page.getCells(page.getRow(row.y));
    return cells[cells.len - 1].wide == .spacer_head;
}

/// Whether the row's first cell has a background color. Printing it is
/// what makes the terminal wrap onto the row, and a wrap at the bottom of
/// the screen scrolls, filling the new row with the current background.
fn startsWithBackground(row: Pin) bool {
    const page = row.node.page();
    const cell = page.getCells(page.getRow(row.y))[0];
    return switch (cell.content_tag) {
        .bg_color_palette, .bg_color_rgb => true,
        .codepoint, .codepoint_grapheme => cell.hasStyling() and
            page.styles.get(page.memory, cell.style_id).bg_color != .none,
    };
}

/// Wrap onto the row with the default pen, by printing its first
/// character (a wide one, which wraps by itself after a spacer head), and
/// step back over it so the row is then written as usual.
fn writeWrapPrimer(w: *Writer, row: Pin) Writer.Error!void {
    const page = row.node.page();
    const cell = page.getCells(page.getRow(row.y))[0];
    try w.writeAll("\x1b[0m");
    if (cell.hasText()) {
        try w.printUnicodeCodepoint(cell.codepoint());
        if (cell.content_tag == .codepoint_grapheme) {
            if (page.lookupGrapheme(&page.getCells(page.getRow(row.y))[0])) |cps| for (cps) |cp| try w.printUnicodeCodepoint(cp);
        }
    } else {
        try w.writeByte(' ');
    }
    try w.writeAll(if (cell.wide == .wide) "\x08\x08" else "\x08");
}

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
    const prev_cells = prev_page.getCells(prev_row);
    const first = page.getCells(this_row)[0];
    // A row ending where a wide character did not fit leaves the cursor
    // before the edge, so only that wide character wraps by itself. With
    // left and right margins, the continuation can start with older text.
    if (prev_cells[prev_cells.len - 1].wide == .spacer_head and first.wide != .wide) return false;
    // The formatter writes nothing for a row with neither text nor a
    // background-only cell, whatever its other styles.
    if (!rowPrints(page.getCells(this_row)) or !rowPrints(prev_cells)) return false;
    return endsAtEdge(prev_cells) and printsAsText(first);
}

/// The formatter's test for a row it writes at all.
fn rowPrints(cells: []const Cell) bool {
    for (cells) |*cell| {
        if (cell.hasText()) return true;
        switch (cell.content_tag) {
            .bg_color_palette, .bg_color_rgb => return true,
            .codepoint, .codepoint_grapheme => {},
        }
    }
    return false;
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
fn writeSavedCursor(w: *Writer, t: *const Terminal, screen: *const Screen) Writer.Error!void {
    const saved = screen.saved_cursor orelse return;
    const cols = screen.pages.cols;
    const x = @min(saved.x, cols - 1);
    const y = @min(saved.y, screen.pages.rows - 1);

    try writeCleanSlate(w);
    // The saved cursor made protected at the start (see `savedCarrier`):
    // DECRC sets the cursor's protection without a mode.
    const carried = saved.protected and screen.protected_mode == .off and savedCarrier(t);
    if (carried) try w.writeAll("\x1b8\x1b[?6l");
    // Setting origin mode homes the cursor, so it comes first. No scroll
    // region is set yet, so positions are absolute either way.
    if (saved.origin) try w.writeAll("\x1b[?6h");
    if (saved.pending_wrap and x == cols - 1) {
        const row = activePin(screen, y);
        const start = edgeStart(row, x);
        try writeCup(w, y, start, .{});
        try writeEdgeCell(w, screen, row, start);
    } else if (saved.pending_wrap and x > 0) {
        // Waiting to wrap away from the edge (at a right margin, or left
        // there after the margins were reset): made at a right margin set
        // there for it (which homes the cursor) and dropped again (which
        // does not).
        const row = activePin(screen, y);
        const start = edgeStart(row, x);
        try w.print("\x1b[?69h\x1b[1;{d}s", .{x + 1});
        try writeCup(w, y, start, .{});
        try writeEdgeCell(w, screen, row, start);
        try w.writeAll("\x1b[?69l");
    } else {
        try writeCup(w, y, x, .{});
    }
    try w.print("{f}", .{saved.style.formatterVt()});
    if (saved.protected and !carried) try writeProtect(w, t, screen);
    try writeCharsets(w, &saved.charset);
    try w.writeAll("\x1b7");
    if (saved.pending_wrap) {
        // The space printed for pending wrap on an empty cell is text, which
        // a terminal without wraparound attaches a combining mark to. Saved,
        // the cell can be emptied again (ECH also resets the row's soft
        // wrap, so not on a wrapped row).
        const row = activePin(screen, y);
        const page = row.node.page();
        const r = page.getRow(row.y);
        const cell = page.getCells(r)[x];
        // A cell with only a background is erased with it as the pen.
        const bg: ?Style = switch (cell.content_tag) {
            .codepoint, .codepoint_grapheme => if (cell.isEmpty() and !cell.hasStyling()) .{} else null,
            .bg_color_palette => .{ .bg_color = .{ .palette = cell.content.color_palette.data } },
            .bg_color_rgb => .{ .bg_color = .{ .rgb = .{
                .r = cell.content.color_rgb.r,
                .g = cell.content.color_rgb.g,
                .b = cell.content.color_rgb.b,
            } } },
        };
        if (bg) |style| if (!r.wrap and !cell.protected) {
            try w.print("\x1b[0m{f}\x1b[X\x1b[0m", .{style.formatterVt()});
        };
    }
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
        try writeEdgeCell(w, screen, row, start);
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
fn writeEdgeCell(w: *Writer, screen: *const Screen, row: Pin, x: CellCountInt) Writer.Error!void {
    // Protecting it would set a mode on a screen whose mode is off, and
    // the formatter writes no cell's protection anyway.
    try writeCell(w, row, x, screen.protected_mode != .off);
}

/// Print one cell in its own style and hyperlink (and, with `protect`,
/// protection), then reset them.
fn writeCell(w: *Writer, row: Pin, x: CellCountInt, protect: bool) Writer.Error!void {
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
    const protected = protect and cell.protected;
    if (protected) try w.writeAll("\x1b[1\"q");
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
    if (protected) try w.writeAll("\x1b[0\"q");
    try w.writeAll("\x1b[0m");
}

/// Step 5.4: the live cursor's pen, hyperlink, protection (with the
/// screen's protection mode) and character sets, after the clean slate.
fn writePen(w: *Writer, t: *const Terminal, screen: *const Screen) Writer.Error!void {
    const cursor = &screen.cursor;
    try w.print("{f}", .{cursor.style.formatterVt()});
    // The screen's most recent protection mode, which erases follow even
    // once protection is off (ISO protects from EL and ED, DEC only from
    // their selective forms), then the cursor's protection in it. Written
    // last for the screen, since printing a protected edge cell above
    // switched the mode to DEC.
    // With the mode off, a protected cursor came from the other screen.
    // (The alternate screen's is carried by `Builder.carryProtection`.)
    switch (screen.protected_mode) {
        .iso => try w.writeAll(if (cursor.protected) "\x1bV" else "\x1bV\x1bW"),
        .dec => try w.writeAll(if (cursor.protected) "\x1b[1\"q" else "\x1b[1\"q\x1b[0\"q"),
        .off => if (cursor.protected and screen == t.screens.get(.primary).?) try writeProtect(w, t, screen),
    }
    // After protection, which the other screen can carry over without it.
    if (cursor.hyperlink) |link| switch (link.id) {
        .explicit => |id| try w.print("\x1b]8;id={s};{s}\x1b\\", .{ id, link.uri }),
        .implicit => try w.print("\x1b]8;;{s}\x1b\\", .{link.uri}),
    };
    try writeCharsets(w, &screen.charset);
}

/// Whether `screen` is the alternate screen with its cursor protected and its
/// protection mode off, which the restore carries from the primary screen's
/// cursor (when that one is protected too, in a mode of its own) or from
/// this screen's saved cursor, carried the same way (see `carriedSaved`).
/// Otherwise the cursor is left unprotected (a known gap).
fn carriedProtection(t: *const Terminal, screen: *const Screen) bool {
    const primary = t.screens.get(.primary).?;
    return screen != primary and screen.protected_mode == .off and screen.cursor.protected and
        ((primary.cursor.protected and primary.protected_mode != .off) or carriedSaved(t, screen));
}

/// Whether `screen` is the alternate screen with its saved cursor protected
/// and its protection mode off, which the restore carries from the primary
/// screen's cursor, when that screen has a mode of its own.
fn carriedSaved(t: *const Terminal, screen: *const Screen) bool {
    const primary = t.screens.get(.primary).?;
    const saved = screen.saved_cursor orelse return false;
    return screen != primary and screen.protected_mode == .off and saved.protected and
        (primary.protected_mode != .off or savedCarrier(t));
}

/// Whether the alternate screen's saved cursor, protected with both screens'
/// protection modes off (DECSTR turned the one it came from off), is carried
/// by a saved cursor made at the start: protected on the primary screen,
/// copied to the alternate screen and saved there, then the primary's mode
/// turned off again with DECSTR. Written as the restore's first step, before
/// anything DECSTR resets. Not when the primary screen needs the alternate
/// screen to carry its own (see `writeProtect`), which DECSTR there wipes.
fn savedCarrier(t: *const Terminal) bool {
    if (t.screens.active_key != .alternate) return false;
    const primary = t.screens.get(.primary).?;
    const alt = t.screens.get(.alternate).?;
    const saved = alt.saved_cursor orelse return false;
    const primary_saved = if (primary.saved_cursor) |sc| sc.protected else false;
    return saved.protected and alt.protected_mode == .off and primary.protected_mode == .off and
        !primary.cursor.protected and !primary_saved;
}

/// Protect the cursor in the screen's protection mode. With the mode off,
/// the protection came from the other screen's cursor (47, 1047 and 1049
/// copy the cursor, protection included, and leave each screen's mode), so
/// it is set there, in that screen's mode, and copied back with 47: this
/// screen's mode stays off, and only the hidden alternate screen's
/// changes. The alternate screen's comes from the primary screen the same
/// way, which leaves the primary cursor to be set again.
fn writeProtect(w: *Writer, t: *const Terminal, screen: *const Screen) Writer.Error!void {
    switch (screen.protected_mode) {
        .iso => try w.writeAll("\x1bV"),
        .dec => try w.writeAll("\x1b[1\"q"),
        .off => if (screen == t.screens.get(.primary).?) {
            const other: ghostty.ProtectedMode = if (t.screens.get(.alternate)) |alt| alt.protected_mode else .off;
            try w.writeAll("\x1b[?47h");
            try w.writeAll(if (other == .iso) "\x1bV" else "\x1b[1\"q");
            try w.writeAll("\x1b[?47l");
        } else if (t.screens.get(.primary).?.protected_mode != .off) {
            // The alternate screen's: carried from the primary screen's
            // cursor, which `Builder.carryProtection` then sets back.
            try w.writeAll("\x1b[?47l");
            try w.writeAll(if (t.screens.get(.primary).?.protected_mode == .iso) "\x1bV" else "\x1b[1\"q");
            try w.writeAll("\x1b[?47h");
        },
    }
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
/// default. Insert mode and wraparound are left to the caller, and the
/// mouse modes are written from the effective mouse state.
fn writeModes(w: *Writer, t: *const Terminal, which: modes.Replay) Writer.Error!void {
    for (modes.table) |e| {
        if (e.replay != which) continue;
        switch (e.mode) {
            .insert,
            .wraparound,
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

/// DECSCUSR for the active screen: the style the program chose, or the
/// default where the screen has it (see `writeDefaultCursorStyle`).
fn writeCursorStyle(w: *Writer, t: *const Terminal) Writer.Error!void {
    if (t.cursor.is_default) {
        if (t.screens.active.cursor.cursor_style == t.cursor.default_style) try w.writeAll("\x1b[0 q");
        return;
    }
    const blink = t.modes.get(.cursor_blinking);
    const n: u8 = switch (t.screens.active.cursor.cursor_style) {
        .block, .block_hollow => if (blink) 1 else 2,
        .underline => if (blink) 3 else 4,
        .bar => if (blink) 5 else 6,
    };
    try w.print("\x1b[{d} q", .{n});
}

/// The default cursor style (`CSI 0 SP q`, the client's own) selected
/// while the primary screen keeps a style of its own, before anything is
/// drawn. The default is selected terminal-wide but each screen
/// keeps its own style, so the primary screen can keep a style of its own
/// when the default was chosen on the alternate screen (1049 does not copy
/// the cursor back), and the alternate screen gets it again on entry (47,
/// 1047 and 1049 copy the cursor there). Rebuilt the same way: the
/// primary's style, the default chosen on the alternate screen, and back
/// with 1049, which also restores the saved cursor and its origin mode,
/// all written after this. The alternate screen's is set after `?47h`.
fn writeDefaultCursorStyle(w: *Writer, t: *const Terminal) Writer.Error!void {
    if (!t.cursor.is_default) return;
    const style = t.screens.get(.primary).?.cursor.cursor_style;
    if (style == t.cursor.default_style) return;
    try w.print("\x1b[{d} q\x1b[?47h\x1b[0 q\x1b[?1049l\x1b[?6l", .{steadyStyle(style)});
}

fn steadyStyle(style: ghostty.CursorStyle) u8 {
    return switch (style) {
        .block, .block_hollow => 2,
        .underline => 4,
        .bar => 6,
    };
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
/// DEL does neither: a terminal keeps it in the string, as Ghostty did.
fn writeOscText(w: *Writer, text: []const u8) Writer.Error!void {
    for (text) |c| {
        if (c < 0x20) continue;
        try w.writeByte(c);
    }
}
