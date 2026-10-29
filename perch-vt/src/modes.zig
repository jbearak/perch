// SPDX-License-Identifier: GPL-3.0-only

//! The mode table: every mode Ghostty knows at the pinned commit, with
//! Ghostty's default and Perch's two marks. `prologue_reset` says whether the
//! restore prologue (written by Perch's Rust side) sets the mode to its
//! default. `replay` says whether `perch_vt_restore` writes the model's value
//! when it differs from the default. Both come from this one table, so the
//! prologue and the replay cannot disagree. See "Restore on attach" in
//! docs/design.md.

const std = @import("std");
const ghostty = @import("ghostty-vt");

pub const Replay = enum(u8) {
    /// Never written by a restore. `reason` says why.
    no = 0,
    /// Written with the other terminal state, after the screens.
    yes = 1,
    /// Written before any history or screen content, because it changes
    /// how wide cells are and so where the cursor and wraps land.
    early = 2,
};

pub const Entry = struct {
    mode: ghostty.Mode,
    prologue_reset: bool,
    replay: Replay,
    /// Why the mode is not replayed (or not reset), from the design.
    reason: ?[:0]const u8 = null,

    pub fn tag(self: Entry) ghostty.modes.ModeTag {
        return .fromMode(self.mode);
    }

    /// Ghostty's default for the mode, which is also what its reset
    /// restores and what the prologue sets.
    pub fn default(self: Entry) bool {
        const defaults: ghostty.ModePacked = .{};
        switch (self.mode) {
            inline else => |m| return @field(defaults, @tagName(m)),
        }
    }
};

fn on(mode: ghostty.Mode) Entry {
    return .{ .mode = mode, .prologue_reset = true, .replay = .yes };
}

fn off(mode: ghostty.Mode, reason: [:0]const u8) Entry {
    return .{ .mode = mode, .prologue_reset = false, .replay = .no, .reason = reason };
}

const screen_switch = "switches screens or saves the cursor; the restore chooses the screen and writes both saved cursors itself";
const reports = "makes a capable terminal send reports";

/// In Ghostty's order. A comptime check below fails the build if Ghostty
/// gains or loses a mode, so an upgrade has to decide each new one.
pub const table = [_]Entry{
    // ANSI modes.
    on(.disable_keyboard), // KAM, 2
    on(.insert), // IRM, 4
    on(.send_receive_mode), // SRM, 12
    on(.linefeed), // LNM, 20

    // DEC private modes.
    on(.cursor_keys), // 1
    off(.@"132_column", "column mode resizes the terminal and erases the screen"), // 3
    on(.slow_scroll), // 4
    on(.reverse_colors), // 5
    on(.origin), // 6
    on(.wraparound), // 7
    on(.autorepeat), // 8
    on(.mouse_event_x10), // 9
    on(.cursor_blinking), // 12
    on(.cursor_visible), // 25
    on(.enable_mode_3), // 40
    on(.reverse_wrap), // 45
    off(.alt_screen_legacy, screen_switch), // 47
    on(.keypad_keys), // 66
    on(.backarrow_key_mode), // 67
    on(.enable_left_and_right_margin), // 69
    on(.mouse_event_normal), // 1000
    on(.mouse_event_button), // 1002
    on(.mouse_event_any), // 1003
    on(.focus_event), // 1004
    on(.mouse_format_utf8), // 1005
    on(.mouse_format_sgr), // 1006
    off(.mouse_alternate_scroll, "alternate scroll is a preference of the terminal's user, not state of the program"), // 1007
    on(.mouse_format_urxvt), // 1015
    on(.mouse_format_sgr_pixels), // 1016
    on(.ignore_keypad_with_numlock), // 1035
    on(.alt_esc_prefix), // 1036
    on(.alt_sends_escape), // 1039
    on(.reverse_wrap_extended), // 1045
    off(.alt_screen, screen_switch), // 1047
    off(.save_cursor, screen_switch), // 1048
    off(.alt_screen_save_cursor_clear_enter, screen_switch), // 1049
    on(.bracketed_paste), // 2004
    .{
        .mode = .synchronized_output, // 2026
        .prologue_reset = true,
        .replay = .no,
        .reason = "synchronized output is a handshake with the terminal that was attached; the prologue resets it because a dropped frame can leave a terminal waiting",
    },
    .{ .mode = .grapheme_cluster, .prologue_reset = true, .replay = .early }, // 2027
    off(.report_color_scheme, reports), // 2031
    off(.report_visibility, reports), // 2033
    off(.in_band_size_reports, reports), // 2048
    off(.kitty_paste_events, reports), // 5522
};

comptime {
    @setEvalBranchQuota(100_000);
    const fields = @typeInfo(ghostty.Mode).@"enum".field_names;
    if (fields.len != table.len) @compileError("perch-vt's mode table does not list every Ghostty mode");
    for (fields) |name| {
        var count = 0;
        for (table) |entry| {
            if (std.mem.eql(u8, @tagName(entry.mode), name)) count += 1;
        }
        if (count != 1) @compileError("mode " ++ name ++ " must appear exactly once in perch-vt's mode table");
    }
    for (table) |entry| {
        if ((entry.replay == .no) != (entry.reason != null))
            @compileError("every mode that is not replayed needs a reason, and only those");
        if (entry.replay != .no and !entry.prologue_reset)
            @compileError("a replayed mode must be reset by the prologue first");
    }
}

/// The table as the C interface exports it. Mirrored in
/// `crates/perch/src/vt_ffi.rs`.
pub const CEntry = extern struct {
    value: u16,
    /// 1 for an ANSI mode (`CSI n h`), 0 for a DEC private mode (`CSI ? n h`).
    ansi: u8,
    default: u8,
    prologue_reset: u8,
    /// A `Replay` value.
    replay: u8,
    /// Why the mode is not replayed, or null.
    reason: ?[*:0]const u8,
};

pub const c_table: [table.len]CEntry = blk: {
    var result: [table.len]CEntry = undefined;
    for (table, &result) |entry, *c| {
        const t = entry.tag();
        c.* = .{
            .value = t.value,
            .ansi = @intFromBool(t.ansi),
            .default = @intFromBool(entry.default()),
            .prologue_reset = @intFromBool(entry.prologue_reset),
            .replay = @backingInt(entry.replay),
            .reason = if (entry.reason) |r| r.ptr else null,
        };
    }
    break :blk result;
};

/// The entry for a mode.
pub fn find(mode: ghostty.Mode) Entry {
    for (table) |e| if (e.mode == mode) return e;
    unreachable;
}

const testing = std.testing;

test "the table follows the design's marks" {
    const excluded = [_]ghostty.Mode{
        .@"132_column",       .alt_screen_legacy,                  .alt_screen,
        .save_cursor,         .alt_screen_save_cursor_clear_enter, .synchronized_output,
        .report_color_scheme, .report_visibility,                  .in_band_size_reports,
        .kitty_paste_events,  .mouse_alternate_scroll,
    };
    for (table) |e| {
        const is_excluded = std.mem.indexOfScalar(ghostty.Mode, &excluded, e.mode) != null;
        try testing.expectEqual(is_excluded, e.replay == .no);
    }
    try testing.expectEqual(Replay.early, find(.grapheme_cluster).replay);
    try testing.expect(find(.synchronized_output).prologue_reset);
    try testing.expectEqual(Replay.no, find(.synchronized_output).replay);
    // Every mode the design names for the prologue is reset there.
    for ([_]ghostty.Mode{
        .mouse_event_x10,         .mouse_event_normal, .mouse_event_button,  .mouse_event_any,
        .focus_event,             .mouse_format_utf8,  .mouse_format_sgr,    .mouse_format_urxvt,
        .mouse_format_sgr_pixels, .bracketed_paste,    .synchronized_output,
    }) |m| try testing.expect(find(m).prologue_reset and !find(m).default());
}

test "defaults are Ghostty's" {
    try testing.expect(find(.wraparound).default());
    try testing.expect(find(.cursor_visible).default());
    try testing.expect(find(.send_receive_mode).default());
    try testing.expect(!find(.bracketed_paste).default());
    var t: ghostty.Terminal = try .init(ghostty.TinyIo.init.io(), testing.allocator, .{ .cols = 10, .rows = 2 });
    defer t.deinit(testing.allocator);
    for (table) |e| try testing.expectEqual(t.modes.getDefault(e.mode), e.default());
}

test "C table" {
    try testing.expectEqual(table.len, c_table.len);
    for (c_table) |c| {
        try testing.expect(c.replay <= 2);
        try testing.expectEqual(c.replay == 0, c.reason != null);
    }
}
