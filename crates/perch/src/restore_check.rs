// SPDX-License-Identifier: GPL-3.0-only

//! A restore against a second `perch-vt`: a stream through the supervisor's
//! [`Feeder`] into a model, a restore built from it, and the restore fed to
//! a fresh terminal at the client's size, which must then agree with the
//! model on what the restore promises ([`restore::differences`]); and again
//! after more output reaches both. Self-referential (both sides are Ghostty), so it finds
//! what the restore leaves out or writes in the wrong order, not where
//! Ghostty and other terminals differ. The proptest below and the
//! `restore` fuzz target both run it.

use crate::model::feed::{Feeder, Step};
use crate::restore::{self, Client, Mode};
use crate::vt_ffi::Model;

const SCROLLBACK: u32 = 200;
const SCROLLBACK_BYTES: u64 = 1 << 20;

/// What one check runs.
#[derive(Debug, Clone)]
pub struct Case<'a> {
    pub stream: &'a [u8],
    /// The stream reaches the feeder in two reads, split here.
    pub split: usize,
    pub model: (u16, u16),
    pub client: (u16, u16),
    pub mode: Mode,
    /// Output after the restore, to both.
    pub after: &'a [u8],
}

pub fn check(case: &Case) -> Result<(), String> {
    let err = |e| format!("perch-vt: {e:?}");
    let mut model =
        Model::new(case.model.0, case.model.1, SCROLLBACK, SCROLLBACK_BYTES).map_err(err)?;
    let mut feeder = Feeder::new();
    let split = case.split.min(case.stream.len());
    let feed = |model: &mut Model, feeder: &mut Feeder, bytes: &[u8]| {
        for step in feeder.push(bytes) {
            if let Step::Frame { n, bytes } = step {
                model.feed(&bytes, n);
            }
        }
    };
    feed(&mut model, &mut feeder, &case.stream[..split]);
    feed(&mut model, &mut feeder, &case.stream[split..]);

    let client = Client {
        cols: case.client.0,
        rows: case.client.1,
        scrollback_rows: SCROLLBACK,
    };
    let built = restore::build(
        &mut model,
        case.mode,
        client,
        feeder.tracker().bracketed_paste(),
    )
    .map_err(err)?;
    // Design item for review (see the M2c PR): the restore writes 2027
    // before any content, so text printed before the program turned it on
    // is laid out again with it on. The proptest only turns it on at the
    // start; random input is held to the same.
    if model.mode(2027, false) == Some(true) && text_before_2027(case)? {
        return Ok(());
    }
    // Known gap: after malformed UTF-8, Ghostty can keep a control (DEL, a
    // C1) inside a cell's cluster. Written back, a terminal reads it as a
    // control rather than text, so it cannot round-trip.
    if cells_hold_controls(&model)? {
        return Ok(());
    }
    if unwritable_cursor(&model)? {
        return Ok(());
    }
    let (cols, rows) = built.size;
    // More scrollback than the client keeps, so this terminal's own pruning
    // (by whole pages) cannot hide rows the restore left out.
    let mut terminal = Model::new(cols, rows, 10_000, SCROLLBACK_BYTES).map_err(err)?;
    terminal.feed(&built.bytes, 0);
    let sent = match case.mode {
        Mode::Full => model
            .history_kept(SCROLLBACK, restore::HISTORY_CAP)
            .map_err(err)? as u64,
        Mode::Screen => 0,
    };
    compare(
        &model,
        &terminal,
        case.mode,
        "after the restore",
        true,
        sent,
        true,
        &[],
        false,
    )?;
    let on_alternate = model.active_screen() == crate::vt_ffi::ActiveScreen::Alternate;
    let state = dump_state(&model)?;
    let screens = || {
        ["primary", "alternate"]
            .into_iter()
            .map(|s| &state["screens"][s])
    };
    // Known gap: cells protected before the restore (ISO or DEC) are not
    // protected in the client, because the row formatter does not write
    // protection, so later erases treat them differently, whatever the
    // protection mode is by then (DECSTR turns the mode off, not the
    // cells). Once the model has such cells, only the state right after
    // the restore counts.
    let protected_cells = screens().any(|s| s["protected_cells"] == true);
    // Shell-integration prompt marks (OSC 133) in the cursor, rows and
    // cells are Ghostty state the formatter does not write, and Ghostty
    // acts on them (ED 2 at a prompt scrolls the screen into history
    // instead of clearing it), which a client such as xterm.js does not.
    // Marks made only after the restore reach both alike.
    let prompt_marks = screens().any(|s| {
        s["prompt_marks"] == true
            || s["semantic_content"]
                .as_str()
                .is_some_and(|c| c != "output")
    });
    // Known gap: a row with no text is written as nothing, though it has
    // styled blank cells (such as the blank left where a wide character
    // did not fit, which a wider client finds away from the edge). Text
    // printed on the row later leaves the styles different.
    // The `col:sgr` entries of those cells: the only styles a row may
    // lose for this reason. Columns stay put as later output scrolls rows.
    let blank_styles: Vec<String> = screens()
        .filter_map(|s| s["rows"].as_array())
        .flatten()
        .filter(|r| r[0] == "")
        .flat_map(row_styles)
        .collect();
    // Insert mode moves them along the row as text is printed before them.
    let insert_at_restore = state["modes"]["4"] == true;

    // What reaches the terminal after the restore: the sequence the feeder
    // held back unfinished, then the output.
    let later = [feeder.held(), case.after].concat();
    terminal.feed(&later, 0);
    // A byte at a time, watching the alternate-screen modes and the active
    // screen: Ghostty executes controls inside a CSI, and a mode can stay
    // set after the screen was left, so neither the bytes nor the modes
    // alone say when the program enters the alternate screen.
    let alt_modes = |m: &Model| {
        let modes = [47, 1047, 1049].map(|n| m.mode(n, false) == Some(true));
        let alternate = m.active_screen() == crate::vt_ffi::ActiveScreen::Alternate;
        [modes[0], modes[1], modes[2], alternate]
    };
    let mut reenters = false;
    let mut before = alt_modes(&model);
    for b in case.after {
        feed(&mut model, &mut feeder, &[*b]);
        let now = alt_modes(&model);
        reenters |= now.iter().zip(before).any(|(n, b)| *n && !b);
        before = now;
    }
    // While the feeder holds an unfinished sequence the model is behind the
    // terminal by those bytes, as it should be.
    if !feeder.held().is_empty() {
        return Ok(());
    }
    // Re-entering the alternate screen after a restore made on the primary
    // finds the hidden state the restore did not carry (its saved cursor
    // can bring back character sets too).
    // And the restore enters the alternate screen with 47 whatever mode
    // the program used, so a redundant `?1049h` from a program already
    // there saves, switches and clears in the client but does nothing in
    // the model (noted in the M2c PR for design review).
    if reenters {
        return Ok(());
    }
    // REP (`CSI n b`) repeats the last character printed, which no restore
    // carries.
    if has_rep(&later) || protected_cells || prompt_marks {
        return Ok(());
    }
    // Reverse wraparound follows the screen rows' soft-wrap marks, which
    // the restore cannot always rebuild (see `restore::differences`).
    if model.mode(45, false) == Some(true) || model.mode(1045, false) == Some(true) {
        return Ok(());
    }
    // The rows the restore sent, plus those the output scrolled up, less
    // any it cleared (RIS, `CSI 3 J`): what the terminal now holds, whose
    // scrollback never prunes them here and whose count was exactly `sent`
    // before the output. A client keeps the newest that fit its own
    // scrollback, and those must be the model's newest. (The model prunes
    // by whole pages, so its own count cannot say.)
    let kept = history_rows(&terminal)?.min(u64::from(SCROLLBACK));
    compare(
        &model,
        &terminal,
        case.mode,
        "after more output",
        on_alternate,
        kept,
        false,
        &blank_styles,
        insert_at_restore,
    )
}

fn cells_hold_controls(model: &Model) -> Result<bool, String> {
    let v = dump_state(model)?;
    Ok(["primary", "alternate"].iter().any(|s| {
        v["screens"][s]["rows"].as_array().is_some_and(|rows| {
            rows.iter().any(|r| {
                r[0].as_str()
                    .is_some_and(|t| t.chars().any(|c| c.is_control()))
            })
        })
    }))
}

fn dump_state(model: &Model) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&model.dump().map_err(|e| format!("dump: {e:?}"))?)
        .map_err(|e| format!("dump: {e}"))
}

fn history_rows(model: &Model) -> Result<u64, String> {
    let v = dump_state(model)?;
    Ok(v["screens"]["primary"]["history_rows"]
        .as_u64()
        .unwrap_or(0))
}

/// Whether the model held any text at a moment 2027 was turned on (at
/// the start, or again after RIS or a reset turned it off), found by
/// feeding the stream to a probe a byte at a time (Ghostty ignores some
/// bytes inside a CSI, so the bytes alone cannot say).
fn text_before_2027(case: &Case) -> Result<bool, String> {
    let err = |e| format!("perch-vt: {e:?}");
    let mut probe =
        Model::new(case.model.0, case.model.1, SCROLLBACK, SCROLLBACK_BYTES).map_err(err)?;
    let mut on = false;
    for b in case.stream {
        probe.feed(&[*b], 0);
        let now = probe.mode(2027, false) == Some(true);
        if now && !on {
            let v = dump_state(&probe)?;
            let text = ["primary", "alternate"].iter().any(|s| {
                v["screens"][s]["rows"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r[0] != "" || r[2] != ""))
            });
            if text {
                return Ok(true);
            }
        }
        on = now;
    }
    Ok(false)
}

/// Whether `bytes` might hold a REP: a CSI with a `b` anywhere after it.
/// Ghostty takes bytes that cannot be parameters inside a CSI, and ignores
/// some between ESC and `[`, so this stays conservative.
fn has_rep(bytes: &[u8]) -> bool {
    let csi = bytes
        .iter()
        .position(|&c| c == 0x1b)
        .and_then(|i| bytes[i..].iter().position(|&c| c == b'[').map(|j| i + j))
        .or_else(|| bytes.iter().position(|&c| c == 0x9b));
    csi.is_some_and(|i| bytes[i..].contains(&b'b'))
}

/// Known gaps in writing the live cursor, both reachable only by unusual
/// sequences, where the check stops:
///
/// - In origin mode, a cursor outside the scroll region (put there by
///   restoring a cursor saved elsewhere). CUP counts from the region and
///   DECSTBM homes the cursor; only DECRC could, and the restore keeps the
///   saved cursor for the program's own.
/// - Pending wrap away from the right edge: at an interior right margin, or
///   left at the old margin's column after the margins were reset.
///   Rebuilding it needs a margin there, and setting margins homes the
///   cursor. (A saved cursor's, and the hidden primary screen's, are made
///   before the program's margins are set.)
fn unwritable_cursor(model: &Model) -> Result<bool, String> {
    let v: serde_json::Value =
        serde_json::from_slice(&model.dump().map_err(|e| format!("dump: {e:?}"))?)
            .map_err(|e| format!("dump: {e}"))?;
    let r = &v["scrolling_region"];
    let c = &v["screens"][v["active"].as_str().unwrap_or("primary")]["cursor"];
    let (x, y) = (c["x"].as_u64(), c["y"].as_u64());
    let cols = v["cols"].as_u64().unwrap_or(0);
    if c["pending_wrap"] == true && x != Some(cols.saturating_sub(1)) {
        return Ok(true);
    }
    if v["modes"]["?6"] != true {
        return Ok(false);
    }
    Ok(y < r[0].as_u64() || y > r[1].as_u64() || x < r[2].as_u64() || x > r[3].as_u64())
}

/// `alternate`: whether the alternate screen's state counts. A restore
/// made on the primary screen does not carry the hidden alternate screen
/// (its contents, saved cursor and kitty stack), which the design restores
/// only while the program is on it. `blank_styles`: the styles of blank
/// cells the restore left out (see `check`), which rows may lack.
#[allow(clippy::too_many_arguments)]
fn compare(
    model: &Model,
    terminal: &Model,
    mode: Mode,
    when: &str,
    alternate: bool,
    history_sent: u64,
    exact: bool,
    blank_styles: &[String],
    insert_at_restore: bool,
) -> Result<(), String> {
    let dump = |m: &Model| -> Result<serde_json::Value, String> {
        serde_json::from_slice(&m.dump().map_err(|e| format!("dump: {e:?}"))?)
            .map_err(|e| format!("dump: {e}"))
    };
    let mut expected = dump(model)?;
    expected["history_sent"] = serde_json::json!(history_sent);
    expected["history_exact"] = serde_json::json!(exact);
    let got = dump(terminal)?;
    let diffs: Vec<String> = restore::differences(&expected, &got, mode)
        .into_iter()
        .filter(|d| !known(&expected, d, blank_styles, insert_at_restore))
        .filter(|d| alternate || !d.starts_with("alternate"))
        .collect();
    if diffs.is_empty() {
        Ok(())
    } else {
        Err(format!("{when}:\n  {}", diffs.join("\n  ")))
    }
}

/// Differences the restore is known not to cover yet.
fn known(
    expected: &serde_json::Value,
    diff: &str,
    blank_styles: &[String],
    insert_at_restore: bool,
) -> bool {
    // The blank cell left at the right edge (or right margin) where a wide
    // character did not fit takes the pen in Ghostty; the restore leaves it
    // unstyled.
    let edge = expected["cols"].as_u64().unwrap_or(0);
    let margin = expected["scrolling_region"][3].as_u64().unwrap_or(0) + 1;
    if last_column_style_only(diff, edge) || last_column_style_only(diff, margin) {
        return true;
    }
    // With left and right margins (DECLRMM), lines inserted or deleted
    // inside them around a wide character that did not fit leave styled
    // blank cells the formatter does not write: same text, different
    // styles on blanks.
    // And a row with no text at all is written as nothing when its only
    // styles are on such empty cells (the formatter writes a row only for
    // text or a background-only cell).
    // With grapheme clustering (2027), cells the program printed apart can
    // cluster when the restore writes them in order (a prepend character
    // before a wide one), which moves the styles of the cells involved.
    let margins_or_clusters =
        expected["modes"]["?69"] == true || expected["modes"]["?2027"] == true;
    (margins_or_clusters && same_text(diff, false))
        || same_text(diff, true)
        || styles_left_out(
            diff,
            blank_styles,
            insert_at_restore || expected["modes"]["4"] == true,
        )
}

/// A row's `col:sgr` style entries.
fn row_styles(row: &serde_json::Value) -> Vec<String> {
    row[2]
        .as_str()
        .unwrap_or("")
        .split(' ')
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether a row difference has the same text, wrap and continuation and
/// the terminal lacks only styles of blank cells the restore left out. With
/// `moved` (insert mode was on), those cells may have moved along the row,
/// so each missing style takes a left-out cell's style from any column.
fn styles_left_out(diff: &str, blank_styles: &[String], moved: bool) -> bool {
    let Some((a, b)) = row_diff(diff) else {
        return false;
    };
    if a[0] != b[0] || a[1] != b[1] || a[3] != b[3] {
        return false;
    }
    let (model, got) = (row_styles(&a), row_styles(&b));
    if !got.iter().all(|e| model.contains(e)) {
        return false;
    }
    let sgr = |e: &String| {
        e.split_once(':')
            .map_or(String::new(), |(_, s)| s.to_owned())
    };
    let mut left: Vec<String> = blank_styles.to_vec();
    model.iter().filter(|e| !got.contains(e)).all(|e| {
        let i = if moved {
            left.iter().position(|l| sgr(l) == sgr(e))
        } else {
            left.iter().position(|l| l == e)
        };
        i.map(|i| left.swap_remove(i)).is_some()
    })
}

/// The two sides of a row difference (`... row N: [..] != [..]`), each a
/// `[text, wrapped, styled, continuation]` row; `None` for any other
/// difference, so that row-only exceptions never excuse a mode or cursor.
fn row_diff(diff: &str) -> Option<(serde_json::Value, serde_json::Value)> {
    let (_, rest) = diff.split_once(": ")?;
    let (a, b) = rest.split_once(" != ")?;
    let a: serde_json::Value = serde_json::from_str(a).ok()?;
    let b: serde_json::Value = serde_json::from_str(b).ok()?;
    let row = |v: &serde_json::Value| v.as_array().is_some_and(|r| r.len() == 4);
    (row(&a) && row(&b)).then_some((a, b))
}

/// Whether a row difference has the same text on both sides (and, with
/// `blank`, no text at all).
fn same_text(diff: &str, blank: bool) -> bool {
    row_diff(diff).is_some_and(|(a, b)| a[0] == b[0] && (!blank || a[0] == ""))
}

/// Whether a row difference (`... row N: [..] != [..]`) is only in the
/// style of the last column and the soft wrap.
fn last_column_style_only(diff: &str, cols: u64) -> bool {
    let Some((a, b)) = row_diff(diff) else {
        return false;
    };
    let last = format!("{}:", cols.saturating_sub(1));
    let strip = |v: &serde_json::Value| -> String {
        v[2].as_str()
            .unwrap_or("")
            .split(' ')
            .filter(|e| !e.starts_with(&last))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // Nor does it rebuild the wrap there when the cell before the spacer
    // is blank: nothing printed reaches the edge.
    a[0] == b[0] && strip(&a) == strip(&b)
}

/// Reads fuzz input: four bytes for the model's and the client's sizes
/// (2 to 41 columns, 1 to 12 rows; a one-column terminal cannot hold a wide
/// character, and how Ghostty copes is out of scope), one for the mode and
/// split, then the
/// stream, and after a `0xff 0xfe` the output that follows the restore.
pub fn check_fuzz_input(data: &[u8]) -> Result<(), String> {
    if data.len() < 5 {
        return Ok(());
    }
    let cols = |b: u8| u16::from(b % 40) + 2;
    let rows = |b: u8| u16::from(b % 12) + 1;
    let (head, body) = data.split_at(5);
    let (stream, after) = match body.windows(2).position(|w| w == b"\xff\xfe") {
        Some(i) => (&body[..i], &body[i + 2..]),
        None => (body, &b""[..]),
    };
    check(&Case {
        stream,
        split: usize::from(head[4] >> 1) * stream.len() / 128,
        model: (cols(head[0]), rows(head[1])),
        client: (cols(head[2]), rows(head[3])),
        mode: if head[4] & 1 == 0 {
            Mode::Full
        } else {
            Mode::Screen
        },
        after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_left_out_blank_styles_may_be_missing() {
        let blank = vec!["7:1".to_owned()];
        let row = |s: &str| {
            format!(r#"primary row 0: ["ab",false,"0:31 {s}",false] != ["ab",false,"0:31",false]"#)
        };
        // The blank cell's style, lost: known.
        assert!(styles_left_out(&row("7:1"), &blank, false));
        // Another cell's style, or the same column with another style: not.
        assert!(!styles_left_out(&row("5:1"), &blank, false));
        assert!(!styles_left_out(&row("7:4"), &blank, false));
        // A mode difference is never a row difference.
        assert!(!styles_left_out("mode ?7: true != false", &blank, true));
        // Nothing left out at the restore: nothing excused.
        assert!(!styles_left_out(&row("7:1"), &[], false));
        // Moved by insert mode: the same style in another column, once.
        assert!(styles_left_out(&row("5:1"), &blank, true));
        assert!(!styles_left_out(&row("5:1 6:1"), &blank, true));
        assert!(!styles_left_out(&row("5:4"), &blank, true));
    }

    #[test]
    fn row_exceptions_never_excuse_other_differences() {
        // Every exception on, as with margins and 2027 set.
        let expected = serde_json::json!({ "cols": 8, "modes": { "?69": true, "?2027": true } });
        let blank = vec!["7:1".to_owned()];
        for diff in [
            "mode ?7: true != false",
            "mode ?2004: false != true",
            "primary cursor: [1,2] != [1,3]",
            r#"primary row 0: "ab" != "ab""#,
        ] {
            assert!(!known(&expected, diff, &blank, true), "{diff}");
        }
    }
    use proptest::prelude::*;

    /// Pieces that reach most of what a restore writes: text of every
    /// width, line feeds and wraps, the pen, cursor moves, both saved
    /// cursors, both screens in every mode, margins, origin mode, tabs,
    /// character sets, protection, hyperlinks, kitty stacks, palette,
    /// titles, the replayed modes, resets, and unfinished sequences.
    fn piece() -> impl Strategy<Value = Vec<u8>> {
        #[rustfmt::skip]
        const PIECES: &[&[u8]] = &[
            b"a", b"word ", b"0123456789", b"\xe4\xb8\xad", b"\xf0\x9f\x98\x80", b"e\xcc\x81",
            b"\xe2\x9d\xa4\xef\xb8\x8f", b"\r\n", b"\n", b"\r", b"\x08", b"\t",
            b"\x1b[1m", b"\x1b[3;4:3m", b"\x1b[31m", b"\x1b[38;5;200m", b"\x1b[48;2;1;2;3m",
            b"\x1b[0m", b"\x1b[7;9;53m", b"\x1b[58;5;3m",
            b"\x1b[H", b"\x1b[3;5H", b"\x1b[10C", b"\x1b[2A", b"\x1b[J", b"\x1b[1K", b"\x1b[2J",
            b"\x1b[3L", b"\x1b[2M", b"\x1b[2@", b"\x1b[2P", b"\x1b[S", b"\x1b[T",
            b"\x1b7", b"\x1b8", b"\x1b[s", b"\x1b[u",
            b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?47h", b"\x1b[?47l", b"\x1b[?1047h",
            b"\x1b[?1047l", b"\x1b[?1048h", b"\x1b[?1048l",
            b"\x1b[2;5r", b"\x1b[r", b"\x1b[?69h", b"\x1b[3;9s", b"\x1b[?69l", b"\x1b[?6h",
            b"\x1b[?6l", b"\x1b[?7l", b"\x1b[?7h", b"\x1b[4h", b"\x1b[4l",
            b"\x1bH", b"\x1b[g", b"\x1b[3g",
            b"\x1b(0", b"\x1b(B", b"\x1b)0", b"\x0e", b"\x0f", b"\x1bn", b"lqk",
            b"\x1b[1\"q", b"\x1b[0\"q", b"\x1bV", b"\x1bW", b"\x1b[2\"q",
            b"\x1b]8;;http://a\x1b\\", b"\x1b]8;id=x;http://b\x07", b"\x1b]8;;\x1b\\",
            b"\x1b[>1u", b"\x1b[>0u", b"\x1b[>5u", b"\x1b[<u", b"\x1b[<3u", b"\x1b[=3;2u",
            b"\x1b]4;1;rgb:10/20/30\x1b\\", b"\x1b]104;1\x07", b"\x1b]10;rgb:ff/00/00\x07",
            b"\x1b]2;title\x07", b"\x1b]7;file:///tmp\x07",
            b"\x1b[?1h", b"\x1b=", b"\x1b[?2004h", b"\x1b[?1000h", b"\x1b[?1006h",
            b"\x1b[?25l", b"\x1b[?12h", b"\x1b[5 q",
            b"\x1b[>4;2m", b"\x1b[?1004h", b"\x1b[?45h", b"\x1b[20h",
            b"\x1b[!p", b"\x1bc", b"\x1b[6n", b"\x1b[", b"\x1b]2;x", b"\xe4",
        ];
        prop_oneof![
            8 => proptest::sample::select(PIECES).prop_map(<[u8]>::to_vec),
            1 => any::<u8>().prop_map(|b| vec![b]),
        ]
    }

    fn mode() -> impl Strategy<Value = Mode> {
        prop_oneof![Just(Mode::Full), Just(Mode::Screen)]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        #[test]
        fn restore_matches_in_a_second_terminal(
            pieces in proptest::collection::vec(piece(), 0..80),
            after in proptest::collection::vec(piece(), 0..10),
            split in 0usize..400,
            cols in 2u16..30,
            rows in 1u16..10,
            mode in mode(),
            graphemes in any::<bool>(),
            client in proptest::option::of((2u16..30, 1u16..10)),
        ) {
            // 2027 only from the start: the restore writes it before any
            // content, so text printed before a program turned it on is
            // laid out again with it on (see the PR for M2c).
            let mut stream = if graphemes { b"\x1b[?2027h".to_vec() } else { Vec::new() };
            stream.extend(pieces.concat());
            let after = after.concat();
            let case = Case {
                stream: &stream,
                split,
                model: (cols, rows),
                client: client.unwrap_or((cols, rows)),
                mode,
                after: &after,
            };
            if let Err(e) = check(&case) {
                prop_assert!(false, "{e}\ncase: {case:?}");
            }
        }
    }

    /// The fuzz target's seed corpus (fuzz/seed-restore.sh): every
    /// fidelity fixture's stream, at a small size and at its own.
    #[test]
    fn fixture_streams_pass() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fidelity/fixtures");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path().join("stream.bin");
            let Ok(stream) = std::fs::read(&path) else {
                continue;
            };
            let stream = &stream[..stream.len().min(1 << 16)];
            for (size, mode) in [
                ((38, 22), Mode::Full),
                ((80, 24), Mode::Screen),
                ((80, 24), Mode::Full),
            ] {
                let case = Case {
                    stream,
                    split: stream.len() / 2,
                    model: size,
                    client: size,
                    mode,
                    after: b"",
                };
                if let Err(e) = check(&case) {
                    panic!("{}: {e}", path.display());
                }
            }
            n += 1;
        }
        assert!(n > 20, "found {n} fixtures in {}", dir.display());
    }

    /// Inputs the `restore` fuzz target found, kept in
    /// fuzz/regressions/restore/ (and in the seed corpus).
    #[test]
    fn fuzz_regressions_pass() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions/restore");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let data = std::fs::read(&path).unwrap();
            if let Err(e) = check_fuzz_input(&data) {
                panic!("{}: {e}", path.display());
            }
            n += 1;
        }
        assert!(n > 0);
    }

    /// What the check stops at is found in the model's cells and rows, not
    /// from its modes alone.
    #[test]
    fn omitted_state_is_found_in_cells() {
        let mut m = Model::new(10, 3, SCROLLBACK, SCROLLBACK_BYTES).unwrap();
        let flags = |m: &Model| {
            let v = dump_state(m).unwrap();
            let s = &v["screens"]["primary"];
            (
                s["protected_mode"].clone(),
                s["protected_cells"].clone(),
                s["prompt_marks"].clone(),
                s["semantic_content"].clone(),
            )
        };
        assert_eq!(
            flags(&m),
            ("off".into(), false.into(), false.into(), "output".into())
        );
        // DECSTR turns protection off, not the cells.
        m.feed(b"\x1b[1\"qab\x1b[!p", 0);
        assert_eq!(
            flags(&m),
            ("off".into(), true.into(), false.into(), "output".into())
        );
        // A prompt's row stays marked once its command's output starts.
        m.feed(b"\x1b]133;A\x07$ \x1b]133;C\x07\r\nout", 0);
        assert_eq!(
            flags(&m),
            ("off".into(), true.into(), true.into(), "output".into())
        );
    }

    #[test]
    fn fuzz_input_is_read() {
        check_fuzz_input(b"").unwrap();
        check_fuzz_input(b"\x13\x07\x13\x07\x00hello\x1b[31m\xff\xfeworld").unwrap();
    }
}
