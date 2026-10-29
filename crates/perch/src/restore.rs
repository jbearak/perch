// SPDX-License-Identifier: GPL-3.0-only

//! Restore on attach, Perch's side of it ("Restore on attach" in
//! docs/design.md):
//!
//! - the prologue that puts any terminal into a known state, and the
//!   epilogue a client writes to its own terminal when it leaves, both
//!   generated from `perch-vt`'s mode table;
//! - `full` and `screen` restores: the prologue, for `full` the clear of
//!   step 2, then what `perch_vt_restore` builds, which starts with the
//!   modes replayed early (2027) so that they precede any content;
//! - the partial-model restore (prologue, notice and resize nudge), which
//!   is also the fallback when a restore passes its byte limit.
//!
//! What the model is fed, and what is held back from it, is
//! [`crate::model::feed::Feeder`]'s: the bytes it holds are what a client
//! gets right after a restore.

use crate::vt_ffi::{ModeEntry, Model, RestoreLimits, RestoreMode, VtError, mode_table};

/// The most history bytes a full restore sends, newest lines kept.
pub const HISTORY_CAP: usize = 4 << 20;
/// The most bytes for everything in a restore but the history.
pub const BYTE_CAP: usize = 32 << 20;
/// Screens are clamped to this many columns and rows.
pub const MAX_CELLS: u16 = 500;

/// The line a restore from a partial model shows in place of the screen.
pub const NOTICE: &[u8] =
    b"[perch] The screen could not be restored. Asking the program to redraw.\r\n";

/// The restore a client asks for in its attach frame. `none` sends nothing
/// and never gets here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Prologue, history, screens and state. For terminals that start empty.
    Full,
    /// Prologue, screens and state; the terminal keeps its scrollback.
    Screen,
}

/// The attaching client's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    pub cols: u16,
    pub rows: u16,
    /// The most history rows it keeps, at its width.
    pub scrollback_rows: u32,
}

impl Client {
    /// The client's size clamped to 1..=500 in each direction, the size the
    /// model and the PTY take.
    pub fn size(&self) -> (u16, u16) {
        (self.cols.clamp(1, MAX_CELLS), self.rows.clamp(1, MAX_CELLS))
    }
}

/// Two PTY resizes that make the kernel deliver SIGWINCH even when the
/// size has not changed: to `via`, then back to `back`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nudge {
    pub via: (u16, u16),
    pub back: (u16, u16),
}

impl Nudge {
    /// One row fewer and back, or one row more for a one-row terminal.
    pub fn for_size(cols: u16, rows: u16) -> Nudge {
        let via = if rows <= 1 { rows + 1 } else { rows - 1 };
        Nudge {
            via: (cols, via),
            back: (cols, rows),
        }
    }
}

/// A restore, ready to send ahead of live output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    pub bytes: Vec<u8>,
    /// The size the PTY must have once the restore is sent. The model is
    /// already at it.
    pub size: (u16, u16),
    /// Set for a partial-model restore: the PTY resizes that ask the
    /// program to redraw, in place of the screen Perch could not send.
    pub nudge: Option<Nudge>,
}

/// The prologue for a client `cols` wide (for its tab stops): steps 1 to 4
/// of "Restore on attach".
pub fn prologue(cols: u16) -> Vec<u8> {
    let mut out = leave(cols, &mode_table());
    // 4. Home the cursor and save it.
    out.extend_from_slice(b"\x1b[H\x1b7");
    out
}

/// What `perch attach` writes to its own terminal when it leaves: steps 1
/// to 3 of the prologue, without homing or saving the cursor. Setting the
/// tab stops moves the cursor along its row, so it ends with a carriage
/// return.
pub fn epilogue(cols: u16) -> Vec<u8> {
    let mut out = leave(cols, &mode_table());
    out.push(b'\r');
    out
}

/// Steps 1 to 3.
fn leave(cols: u16, table: &[ModeEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    // 1. Abort an open sequence or string, close a hyperlink.
    out.extend_from_slice(b"\x18\x1b]8;;\x1b\\");
    // 2. Pop the alternate screen's kitty stack, leave it, pop the main
    // screen's.
    out.extend_from_slice(b"\x1b[<99u\x1b[?1049l\x1b[<99u");
    // 3. DECSTR, then explicit resets for terminals that implement it
    // partly or not at all.
    out.extend_from_slice(
        b"\x1b[!p\x1b[r\x1b[0m\x1b[4l\x1b[20l\x1b[?6l\x1b[?7h\x1b[?1l\x1b(B\x1b)B\x0f\x1b[?25h\x1b>\x1b[>4m",
    );
    for e in table.iter().filter(|e| e.prologue_reset) {
        out.extend_from_slice(&mode_bytes(e, e.default));
    }
    // Default tab stops: Ghostty's, every 8 columns, none at the edge.
    out.extend_from_slice(b"\x1b[3g");
    let cols = cols.clamp(1, MAX_CELLS);
    let mut col = 8;
    while col + 1 < cols {
        out.extend_from_slice(format!("\x1b[{}G\x1bH", col + 1).as_bytes());
        col += 8;
    }
    out.extend_from_slice(b"\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
    out
}

/// `CSI [?] n h` or `l`.
fn mode_bytes(e: &ModeEntry, on: bool) -> Vec<u8> {
    format!(
        "\x1b[{}{}{}",
        if e.ansi { "" } else { "?" },
        e.value,
        if on { 'h' } else { 'l' }
    )
    .into_bytes()
}

/// Build a restore from the model for `client`. The model is resized to
/// the client's size; resizing the PTY to [`Restore::size`] is the
/// caller's, after. A restore past its byte limit falls back to the
/// partial-model restore; `bracketed_paste` is the tracker's view, which
/// that fallback replays.
pub fn build(
    model: &mut Model,
    mode: Mode,
    client: Client,
    bracketed_paste: bool,
) -> Result<Restore, VtError> {
    let limits = RestoreLimits {
        client_scrollback_rows: client.scrollback_rows,
        history_cap: HISTORY_CAP,
        byte_cap: BYTE_CAP,
    };
    build_with_limits(model, mode, client, bracketed_paste, limits)
}

/// [`build`] with other limits, for tests of the byte cap.
pub fn build_with_limits(
    model: &mut Model,
    mode: Mode,
    client: Client,
    bracketed_paste: bool,
    limits: RestoreLimits,
) -> Result<Restore, VtError> {
    let (cols, rows) = client.size();
    let vt_mode = match mode {
        Mode::Full => RestoreMode::Full,
        Mode::Screen => RestoreMode::Screen,
    };
    let body = match model.restore(vt_mode, cols, rows, limits) {
        Ok(body) => body,
        Err(VtError::TooLarge) => return Ok(partial(client, bracketed_paste)),
        Err(e) => return Err(e),
    };
    let mut bytes = prologue(cols);
    if mode == Mode::Full {
        // 2. Clear the screen and the terminal's scrollback. What
        // perch-vt wrote starts with the early modes (2027), so they land
        // here, before any content.
        bytes.extend_from_slice(b"\x1b[H\x1b[2J\x1b[3J");
    }
    bytes.extend_from_slice(&sixteen_colors(&body));
    Ok(Restore {
        bytes,
        size: (cols, rows),
        nudge: None,
    })
}

/// Rewrites `CSI 38;5;N m` and `CSI 48;5;N m` for N below 16 in the 16-color
/// form (`CSI 3N m`, `CSI 9N m`, and 4N or 10N for the background). Ghostty
/// stores both forms as the same palette index and its formatter writes the
/// 256-color one, but xterm.js draws bold text in bright colors only for
/// the 16-color form, so a bold `CSI 1;31 m` prompt would come back dull.
/// Programs mostly use the 16-color form for these colors; one that wrote
/// `CSI 38;5;1 m` with bold gets the bright color back instead. Nothing
/// else in a restore can hold these bytes: text never contains ESC, and
/// OSC strings are written without controls.
pub fn sixteen_colors(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if let Some((sgr, len)) = palette_sgr(&body[i..]) {
            out.extend_from_slice(sgr.as_bytes());
            i += len;
        } else {
            out.push(body[i]);
            i += 1;
        }
    }
    out
}

/// `ESC [ 38;5;N m` or `ESC [ 48;5;N m` with N below 16, as the 16-color
/// SGR and the length matched.
fn palette_sgr(b: &[u8]) -> Option<(String, usize)> {
    let rest = b.strip_prefix(b"\x1b[")?;
    let (fg, rest) = if let Some(r) = rest.strip_prefix(b"38;5;") {
        (true, r)
    } else {
        (false, rest.strip_prefix(b"48;5;")?)
    };
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 2 || rest.get(digits) != Some(&b'm') {
        return None;
    }
    let n: u8 = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
    if n >= 16 || (digits == 2 && rest[0] == b'0') {
        return None;
    }
    let code = match (fg, n < 8) {
        (true, true) => 30 + n as u32,
        (true, false) => 90 + n as u32 - 8,
        (false, true) => 40 + n as u32,
        (false, false) => 100 + n as u32 - 8,
    };
    Some((format!("\x1b[{code}m"), 2 + 5 + digits + 1))
}

/// The restore from a partial model, and the fallback past the byte limit:
/// the prologue, a one-line notice, bracketed paste if the program has it
/// on, and a resize nudge so the program redraws.
pub fn partial(client: Client, bracketed_paste: bool) -> Restore {
    let (cols, rows) = client.size();
    let mut bytes = prologue(cols);
    bytes.extend_from_slice(NOTICE);
    if bracketed_paste {
        bytes.extend_from_slice(b"\x1b[?2004h");
    }
    Restore {
        bytes,
        size: (cols, rows),
        nudge: Some(Nudge::for_size(cols, rows)),
    }
}

/// How a terminal fed a restore differs from the model it was built from,
/// both seen through `perch_vt_dump`: one line per difference, empty when
/// they agree. This is the second-`perch-vt` oracle, and it is
/// self-referential: both sides are Ghostty. It compares what the restore
/// promises to rebuild: modes the restore replays (not the screen
/// switches or the others it leaves out), margins, tab stops, palette and
/// colors, title, working directory, cursor style, and each screen's
/// cursor, pen, character sets, saved cursor, kitty stack and rows. An
/// alternate screen counts only while it is active, and a `screen`
/// restore is compared without history. A full restore is compared on
/// exactly the newest `history_sent` history rows when `expected` has that
/// field (see `Model::history_kept`). Soft wrapping is compared on
/// history rows only, except where the restore cannot rebuild it: at the
/// boundary with the screen, and around blank cells.
pub fn differences(
    expected: &serde_json::Value,
    got: &serde_json::Value,
    mode: Mode,
) -> Vec<String> {
    use serde_json::Value;
    let mut out = Vec::new();
    let table = mode_table();
    for (key, e) in expected.as_object().into_iter().flatten() {
        match key.as_str() {
            "modes" => {
                for m in table
                    .iter()
                    .filter(|m| m.replay != crate::vt_ffi::Replay::No)
                {
                    let k = format!("{}{}", if m.ansi { "" } else { "?" }, m.value);
                    if e[&k] != got["modes"][&k] {
                        out.push(format!("mode {k}: {} != {}", e[&k], got["modes"][&k]));
                    }
                }
            }
            "screens" => {
                let active = expected["active"].as_str().unwrap_or("primary");
                for name in ["primary", "alternate"] {
                    if name == "alternate" && active != "alternate" {
                        continue;
                    }
                    let (es, gs) = (&e[name], &got["screens"][name]);
                    for (field, ev) in es.as_object().into_iter().flatten() {
                        let gv = &gs[field];
                        match field.as_str() {
                            "rows" | "history_rows" => {}
                            // Never saved restores as the defaults at home,
                            // which is what the prologue saves.
                            "saved_cursor" if ev.is_null() && is_default_saved(gv) => {}
                            _ if ev != gv => out.push(format!("{name}.{field}: {ev} != {gv}")),
                            _ => {}
                        }
                    }
                    let rows = expected["rows"].as_u64().unwrap_or(0) as usize;
                    let take = |s: &Value| -> Vec<Value> {
                        let all = s["rows"].as_array().cloned().unwrap_or_default();
                        let keep = if mode == Mode::Screen || name == "alternate" {
                            rows.min(all.len())
                        } else {
                            all.len()
                        };
                        all[all.len() - keep..].to_vec()
                    };
                    let (mut er, gr) = (take(es), take(gs));
                    // History limited by the client's scrollback or the
                    // history cap: exactly the newest `history_sent` rows,
                    // when the caller says how many were sent.
                    if name == "primary" && mode == Mode::Full {
                        if let Some(sent) = expected["history_sent"].as_u64() {
                            let keep = (sent as usize + rows).min(er.len());
                            er.drain(..er.len() - keep);
                        } else if gr.len() < er.len() && gr.len() >= rows {
                            er.drain(..er.len() - gr.len());
                        }
                    }
                    if er.len() != gr.len() {
                        out.push(format!("{name}: {} rows != {}", er.len(), gr.len()));
                    }
                    // A logical line that continues from history onto the
                    // screen is split there, and stays split as later
                    // output scrolls it up: one row may differ only in
                    // being soft-wrapped.
                    // A soft-wrapped row that is blank, or whose
                    // continuation is blank or starts with a blank cell, is
                    // written as two lines: the formatter steps over blanks,
                    // so the terminal cannot be made to wrap there.
                    // Two rows: the boundary row's wrap mark and the next
                    // row's continuation mark.
                    let mut split = if name == "primary" && mode == Mode::Full {
                        2
                    } else {
                        0
                    };
                    for (i, (a, b)) in er.iter().zip(&gr).enumerate() {
                        if a == b {
                            continue;
                        }
                        let wrap_only = a[0] == b[0] && a[2] == b[2];
                        // Ghostty can leave a row marked wrapped when the
                        // row after it is no longer its continuation (after
                        // insert-line, say); the restore follows both marks.
                        let next_continues = er.get(i + 1).is_none_or(|n| n[3] == true);
                        if wrap_only && !next_continues {
                            continue;
                        }
                        // Wrapping on the screen itself is not compared:
                        // the contract covers it for history, where joined
                        // lines must rewrap, and Ghostty keeps a row's flag
                        // when its continuation is erased or moved.
                        if wrap_only && i + rows >= er.len() {
                            continue;
                        }
                        let blank_start = |n: &Value| {
                            n[0].as_str()
                                .is_some_and(|t| t.is_empty() || t.starts_with(' '))
                        };
                        if wrap_only && (a[0] == "" || er.get(i + 1).is_some_and(blank_start)) {
                            continue;
                        }
                        if wrap_only && split > 0 {
                            split -= 1;
                            continue;
                        }
                        out.push(format!("{name} row {i}: {a} != {b}"));
                    }
                }
            }
            "history_sent" => {}
            _ if *e != got[key] => out.push(format!("{key}: {e} != {}", got[key])),
            _ => {}
        }
    }
    out
}

fn is_default_saved(v: &serde_json::Value) -> bool {
    *v == serde_json::json!({
        "x": 0, "y": 0, "pending_wrap": false, "protected": false, "origin": false,
        "style": "\x1b[0m",
        "charset": ["ascii", "ascii", "ascii", "ascii", "G0", "G2"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: Client = Client {
        cols: 20,
        rows: 4,
        scrollback_rows: 1000,
    };

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn find(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("{:?} not in output", String::from_utf8_lossy(needle)))
    }

    #[test]
    fn prologue_follows_the_design_order() {
        let p = prologue(80);
        assert!(p.starts_with(b"\x18\x1b]8;;\x1b\\\x1b[<99u\x1b[?1049l\x1b[<99u\x1b[!p"));
        assert!(p.ends_with(b"\x1b[H\x1b7"));
        // Generated from the table: every reset mode, 2026 included, 1049
        // and 1007 not.
        for (mode, there) in [
            (&b"\x1b[?2026l"[..], true),
            (b"\x1b[?2027l", true),
            (b"\x1b[?2004l", true),
            (b"\x1b[?1000l", true),
            (b"\x1b[?7h", true),
            (b"\x1b[?25h", true),
            (b"\x1b[?1007", false),
            (b"\x1b[?3l", false),
            (b"\x1b[?47l", false),
        ] {
            assert_eq!(
                contains(&p, mode),
                there,
                "{}",
                String::from_utf8_lossy(mode)
            );
        }
        // Tab stops at 9, 17, ... 73: none at the last column.
        assert!(contains(&p, b"\x1b[3g\x1b[9G\x1bH"));
        assert!(contains(&p, b"\x1b[73G\x1bH\x1b]104"));
        assert!(!contains(&p, b"\x1b[81G"));
        assert!(contains(
            &p,
            b"\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\"
        ));
    }

    #[test]
    fn epilogue_neither_homes_nor_saves() {
        let e = epilogue(80);
        assert!(e.starts_with(b"\x18"));
        assert!(!contains(&e, b"\x1b7"));
        assert!(!contains(&e, b"\x1b[H"));
        assert!(e.ends_with(b"\x1b\\\r"));
    }

    #[test]
    fn prologue_resets_a_hostile_terminal() {
        let mut m = Model::new(20, 4, 100, 1 << 20).unwrap();
        m.feed(
            b"\x1b[?1049h\x1b[2;3r\x1b[?69h\x1b[5;9s\x1b[4h\x1b[>1u\x1b]8;;http://x\x1b\\\x1b[?2027h\x1b[?2004h\x1b[31",
            0,
        );
        m.feed(&prologue(20), 0);
        let mut fresh = Model::new(20, 4, 100, 1 << 20).unwrap();
        fresh.feed(b"\x1b7", 0);
        assert_eq!(
            differences(&state(&fresh), &state(&m), Mode::Screen),
            Vec::<String>::new()
        );
    }

    fn state(m: &Model) -> serde_json::Value {
        serde_json::from_slice(&m.dump().unwrap()).unwrap()
    }

    #[test]
    fn full_restore_writes_2027_before_content() {
        let mut m = Model::new(20, 4, 100, 1 << 20).unwrap();
        m.feed(b"\x1b[?2027hhistory\r\n1\r\n2\r\n3\r\n4\r\nscreen", 0);
        let r = build(&mut m, Mode::Full, CLIENT, false).unwrap();
        let clear = find(&r.bytes, b"\x1b[H\x1b[2J\x1b[3J");
        let early = find(&r.bytes[clear..], b"\x1b[?2027h") + clear;
        assert!(early < find(&r.bytes, b"history"));
        assert_eq!(r.nudge, None);
        assert_eq!(r.size, (20, 4));

        let s = build(&mut m, Mode::Screen, CLIENT, false).unwrap();
        assert!(!contains(&s.bytes, b"\x1b[3J"));
        assert!(!contains(&s.bytes, b"history"));
        assert!(find(&s.bytes, b"\x1b[?2027h") < find(&s.bytes, b"screen"));
    }

    #[test]
    fn restore_into_a_second_model_matches() {
        let mut m = Model::new(20, 4, 100, 1 << 20).unwrap();
        m.feed(
            b"a\r\nb\r\nc\r\n\x1b[>1u\x1b[>3u\x1b[1;31mred\x1b7\x1b[?1049h\x1b[>5ualt\x1b[2;2H",
            0,
        );
        let r = build(&mut m, Mode::Full, CLIENT, false).unwrap();
        let mut b = Model::new(20, 4, 100, 1 << 20).unwrap();
        b.feed(&r.bytes, 0);
        assert_eq!(
            differences(&state(&m), &state(&b), Mode::Full),
            Vec::<String>::new()
        );
    }

    #[test]
    fn byte_cap_falls_back_to_the_partial_restore() {
        let mut m = Model::new(20, 4, 100, 1 << 20).unwrap();
        m.feed(b"\x1b[31mred", 0);
        let limits = RestoreLimits {
            client_scrollback_rows: 1000,
            history_cap: HISTORY_CAP,
            byte_cap: 4,
        };
        let r = build_with_limits(&mut m, Mode::Full, CLIENT, true, limits).unwrap();
        let mut want = prologue(20);
        want.extend_from_slice(NOTICE);
        want.extend_from_slice(b"\x1b[?2004h");
        assert_eq!(r.bytes, want);
        assert_eq!(
            r.nudge,
            Some(Nudge {
                via: (20, 3),
                back: (20, 4)
            })
        );
        assert_eq!(Nudge::for_size(5, 1).via, (5, 2));
        assert_eq!(partial(CLIENT, false).bytes.len(), want.len() - 8);
    }

    #[test]
    fn palette_colors_below_16_use_the_16_color_form() {
        assert_eq!(
            sixteen_colors(
                b"a\x1b[38;5;1mb\x1b[48;5;15mc\x1b[38;5;16m\x1b[38;5;01m\x1b[58;5;1m\x1b[38;5;9"
            ),
            b"a\x1b[31mb\x1b[107mc\x1b[38;5;16m\x1b[38;5;01m\x1b[58;5;1m\x1b[38;5;9"
        );
        assert_eq!(
            sixteen_colors(b"\x1b[48;5;0m\x1b[38;5;8m"),
            b"\x1b[40m\x1b[90m"
        );
    }

    #[test]
    fn clamps_the_client_size() {
        let c = Client {
            cols: 0,
            rows: 900,
            scrollback_rows: 0,
        };
        assert_eq!(c.size(), (1, 500));
    }
}
