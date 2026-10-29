// SPDX-License-Identifier: GPL-3.0-only

//! A model's state as JSON, for tests: what `perch-testprogs model-dump`
//! writes as the expected state of a restore, and what the restore tests
//! compare between the model and a second terminal fed the restore. It
//! covers what the fidelity contract names and what xterm.js does not
//! expose: both screens' cursors, saved cursors, character sets and kitty
//! stacks, every mode in the table, margins, tab stops, the palette, and
//! each row's text.
//!
//! Not part of the restore path. Styles are written as the SGR sequence
//! Ghostty's VT formatter would emit for them, so two dumps compare equal
//! exactly when the styles do.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Writer = std.Io.Writer;
const ghostty = @import("ghostty-vt");
const modes = @import("modes.zig");

const Terminal = ghostty.Terminal;
const Screen = ghostty.Screen;
const Pin = ghostty.Pin;

/// The caller owns the result.
pub fn dump(alloc: Allocator, t: *const Terminal) Allocator.Error![]u8 {
    var aw: Writer.Allocating = .init(alloc);
    defer aw.deinit();
    write(&aw.writer, t) catch return error.OutOfMemory;
    return aw.toOwnedSlice();
}

fn write(w: *Writer, t: *const Terminal) Writer.Error!void {
    try w.print("{{\"cols\":{d},\"rows\":{d},\"active\":\"{s}\"", .{
        t.cols, t.rows, @tagName(t.screens.active_key),
    });

    try w.writeAll(",\"modes\":{");
    for (modes.table, 0..) |e, i| {
        const tag = e.tag();
        if (i > 0) try w.writeByte(',');
        try w.print("\"{s}{d}\":{}", .{ if (tag.ansi) "" else "?", tag.value, t.modes.get(e.mode) });
    }
    try w.writeByte('}');

    try w.print(",\"mouse_event\":\"{s}\",\"mouse_format\":\"{s}\",\"modify_other_keys_2\":{}", .{
        @tagName(t.flags.mouse_event), @tagName(t.flags.mouse_format), t.flags.modify_other_keys_2,
    });

    const r = t.scrolling_region;
    try w.print(",\"scrolling_region\":[{d},{d},{d},{d}]", .{ r.top, r.bottom, r.left, r.right });

    try w.writeAll(",\"tabstops\":[");
    var first = true;
    for (0..t.cols) |col| {
        if (!t.tabstops.get(col)) continue;
        if (!first) try w.writeByte(',');
        first = false;
        try w.print("{d}", .{col});
    }
    try w.writeByte(']');

    try w.writeAll(",\"palette\":{");
    first = true;
    const palette = &t.colors.palette;
    for (0..palette.current.len) |i| {
        if (!palette.mask.isSet(i)) continue;
        if (!first) try w.writeByte(',');
        first = false;
        const rgb = palette.current[i];
        try w.print("\"{d}\":\"{x:0>2}{x:0>2}{x:0>2}\"", .{ i, rgb.r, rgb.g, rgb.b });
    }
    try w.writeByte('}');
    for ([_]struct { []const u8, ghostty.color.DynamicRGB }{
        .{ "foreground", t.colors.foreground },
        .{ "background", t.colors.background },
        .{ "cursor_color", t.colors.cursor },
    }) |pair| {
        try w.print(",\"{s}\":", .{pair[0]});
        if (pair[1].override) |rgb| {
            try w.print("\"{x:0>2}{x:0>2}{x:0>2}\"", .{ rgb.r, rgb.g, rgb.b });
        } else try w.writeAll("null");
    }

    try w.writeAll(",\"title\":");
    try writeOptString(w, t.getTitle());
    try w.writeAll(",\"pwd\":");
    try writeOptString(w, t.getPwd());
    try w.print(",\"cursor_style\":\"{s}\",\"cursor_style_default\":{}", .{
        @tagName(t.screens.active.cursor.cursor_style), t.cursor.is_default,
    });

    try w.writeAll(",\"screens\":{");
    try w.writeAll("\"primary\":");
    try writeScreen(w, t.screens.get(.primary).?);
    if (t.screens.get(.alternate)) |alt| {
        try w.writeAll(",\"alternate\":");
        try writeScreen(w, alt);
    }
    try w.writeAll("}}\n");
}

fn writeScreen(w: *Writer, s: *const Screen) Writer.Error!void {
    const c = &s.cursor;
    try w.print("{{\"cursor\":{{\"x\":{d},\"y\":{d},\"pending_wrap\":{},\"protected\":{},\"style\":", .{
        c.x, c.y, c.pending_wrap, c.protected,
    });
    try writeStyle(w, c.style);
    try w.writeAll(",\"hyperlink\":");
    if (c.hyperlink) |link| {
        try writeString(w, link.uri);
    } else try w.writeAll("null");
    try w.writeAll("},\"charset\":");
    try writeCharset(w, &s.charset);

    try w.writeAll(",\"saved_cursor\":");
    if (s.saved_cursor) |sc| {
        try w.print("{{\"x\":{d},\"y\":{d},\"pending_wrap\":{},\"protected\":{},\"origin\":{},\"style\":", .{
            sc.x, sc.y, sc.pending_wrap, sc.protected, sc.origin,
        });
        try writeStyle(w, sc.style);
        try w.writeAll(",\"charset\":");
        try writeCharset(w, &sc.charset);
        try w.writeByte('}');
    } else try w.writeAll("null");

    // The ring as Ghostty keeps it, oldest first, ending with the current
    // entry: what a restore rebuilds, whatever the index.
    const stack = &s.kitty_keyboard;
    try w.writeAll(",\"kitty\":[");
    for (1..stack.flags.len + 1) |i| {
        if (i > 1) try w.writeByte(',');
        try w.print("{d}", .{stack.flags[(@as(usize, stack.idx) + i) % stack.flags.len].int()});
    }
    try w.writeByte(']');

    const history = s.pages.total_rows - s.pages.rows;
    try w.print(",\"history_rows\":{d},\"rows\":[", .{history});
    var it = s.pages.pin(.{ .screen = .{} }).?.rowIterator(.right_down, null);
    var first = true;
    while (it.next()) |row| {
        if (!first) try w.writeByte(',');
        first = false;
        try writeRow(w, row);
    }
    try w.writeAll("]}");
}

/// `[text, wrapped, styled]`: the row's text with trailing blanks dropped
/// and wide characters once, whether it is soft-wrapped onto the next,
/// and the SGR of each styled cell as `col:sgr` pairs.
fn writeRow(w: *Writer, row: Pin) Writer.Error!void {
    const page = row.node.page();
    const r = page.getRow(row.y);
    const cells = page.getCells(r);
    var buf: [16384]u8 = undefined;
    var text: Writer = .fixed(&buf);
    var keep: usize = 0;
    for (cells) |*cell| {
        switch (cell.wide) {
            .spacer_head, .spacer_tail => continue,
            .narrow, .wide => {},
        }
        if (cell.hasText()) {
            text.printUnicodeCodepoint(cell.codepoint()) catch break;
            if (cell.content_tag == .codepoint_grapheme) {
                if (page.lookupGrapheme(cell)) |cps| for (cps) |cp| {
                    text.printUnicodeCodepoint(cp) catch break;
                };
            }
            keep = text.end;
        } else {
            text.writeByte(' ') catch break;
        }
    }
    try w.writeByte('[');
    try writeString(w, buf[0..keep]);
    try w.print(",{},\"", .{r.wrap});
    for (cells, 0..) |cell, x| {
        if (!cell.hasStyling() and cell.content_tag == .codepoint) continue;
        var sbuf: [256]u8 = undefined;
        var sw: Writer = .fixed(&sbuf);
        switch (cell.content_tag) {
            .codepoint, .codepoint_grapheme => sw.print("{f}", .{page.styles.get(page.memory, cell.style_id).formatterVt()}) catch {},
            .bg_color_palette => sw.print("bg{d}", .{cell.content.color_palette.data}) catch {},
            .bg_color_rgb => sw.print("bg{x:0>2}{x:0>2}{x:0>2}", .{ cell.content.color_rgb.r, cell.content.color_rgb.g, cell.content.color_rgb.b }) catch {},
        }
        try w.print("{d}:", .{x});
        try writeEscaped(w, sw.buffered());
        try w.writeByte(' ');
    }
    try w.writeAll("\"]");
}

fn writeStyle(w: *Writer, style: ghostty.Style) Writer.Error!void {
    var buf: [256]u8 = undefined;
    var sw: Writer = .fixed(&buf);
    sw.print("{f}", .{style.formatterVt()}) catch {};
    try writeString(w, sw.buffered());
}

fn writeCharset(w: *Writer, state: *const Screen.CharsetState) Writer.Error!void {
    try w.writeByte('[');
    // UTF-8 and ASCII print alike; `ESC ( B` designates the second.
    for ([_]ghostty.CharsetSlot{ .G0, .G1, .G2, .G3 }) |slot| {
        const name = switch (state.charsets.get(slot)) {
            .utf8, .ascii => "ascii",
            else => |c| @tagName(c),
        };
        try w.print("\"{s}\",", .{name});
    }
    try w.print("\"{s}\",\"{s}\"]", .{ @tagName(state.gl), @tagName(state.gr) });
}

fn writeOptString(w: *Writer, s: ?[:0]const u8) Writer.Error!void {
    if (s) |v| try writeString(w, v) else try w.writeAll("null");
}

fn writeString(w: *Writer, s: []const u8) Writer.Error!void {
    try w.writeByte('"');
    try writeEscaped(w, s);
    try w.writeByte('"');
}

/// JSON string contents. Bytes that are not valid UTF-8 become U+FFFD so
/// the result always parses.
fn writeEscaped(w: *Writer, s: []const u8) Writer.Error!void {
    var i: usize = 0;
    while (i < s.len) {
        const c = s[i];
        if (c < 0x80) {
            switch (c) {
                '"' => try w.writeAll("\\\""),
                '\\' => try w.writeAll("\\\\"),
                0...0x1f, 0x7f => try w.print("\\u{x:0>4}", .{c}),
                else => try w.writeByte(c),
            }
            i += 1;
            continue;
        }
        const n = std.unicode.utf8ByteSequenceLength(c) catch {
            try w.writeAll("\u{FFFD}");
            i += 1;
            continue;
        };
        if (i + n > s.len or !std.unicode.utf8ValidateSlice(s[i .. i + n])) {
            try w.writeAll("\u{FFFD}");
            i += 1;
            continue;
        }
        try w.writeAll(s[i .. i + n]);
        i += n;
    }
}

const testing = std.testing;

test "dump is JSON with both screens" {
    var t: Terminal = try .init(ghostty.TinyIo.init.io(), testing.allocator, .{ .cols = 10, .rows = 3 });
    defer t.deinit(testing.allocator);
    t.printString("h\"i") catch unreachable;
    const out = try dump(testing.allocator, &t);
    defer testing.allocator.free(out);
    const parsed = try std.json.parseFromSlice(std.json.Value, testing.allocator, out, .{});
    defer parsed.deinit();
    const rows = parsed.value.object.get("screens").?.object.get("primary").?.object.get("rows").?.array;
    try testing.expectEqual(@as(usize, 3), rows.items.len);
    try testing.expectEqualStrings("h\"i", rows.items[0].array.items[0].string);
}
