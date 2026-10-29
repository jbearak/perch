// SPDX-License-Identifier: GPL-3.0-only

//! A restore against a second `perch-vt`: a stream through the gate into a
//! model, a restore built from it, and the restore fed to a fresh terminal
//! at the client's size, which must then agree with the model on what the
//! restore promises ([`restore::differences`]); and again after more output
//! reaches both. Self-referential (both sides are Ghostty), so it finds
//! what the restore leaves out or writes in the wrong order, not where
//! Ghostty and other terminals differ. The proptest below and the
//! `restore` fuzz target both run it.

use crate::restore::{self, Client, Gate, Mode};
use crate::vt_ffi::Model;

const SCROLLBACK: u32 = 200;
const SCROLLBACK_BYTES: u64 = 1 << 20;

/// What one check runs.
#[derive(Debug, Clone)]
pub struct Case<'a> {
    pub stream: &'a [u8],
    /// The stream reaches the gate in two reads, split here.
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
    let mut gate = Gate::new();
    let split = case.split.min(case.stream.len());
    let mut frame = 0;
    let mut feed = |model: &mut Model, gate: &mut Gate, bytes: &[u8]| {
        for chunk in gate.push(bytes).chunks {
            frame += 1;
            model.feed(&chunk.bytes, frame);
        }
    };
    feed(&mut model, &mut gate, &case.stream[..split]);
    feed(&mut model, &mut gate, &case.stream[split..]);

    let client = Client {
        cols: case.client.0,
        rows: case.client.1,
        scrollback_rows: SCROLLBACK,
    };
    let built = restore::build(
        &mut model,
        case.mode,
        client,
        gate.tracker().bracketed_paste(),
    )
    .map_err(err)?;
    if unwritable_cursor(&model)? {
        return Ok(());
    }
    let (cols, rows) = built.size;
    let mut terminal = Model::new(cols, rows, SCROLLBACK, SCROLLBACK_BYTES).map_err(err)?;
    terminal.feed(&built.bytes, 0);
    compare(&model, &terminal, case.mode, "after the restore", true)?;
    let on_alternate = model.active_screen() == crate::vt_ffi::ActiveScreen::Alternate;

    terminal.feed(gate.held(), 0);
    terminal.feed(case.after, 0);
    feed(&mut model, &mut gate, case.after);
    // While the gate holds an unfinished sequence the model is behind the
    // terminal by those bytes, as it should be.
    if !gate.held().is_empty() {
        return Ok(());
    }
    // Re-entering the alternate screen after a restore made on the primary
    // finds the hidden state the restore did not carry (its saved cursor
    // can bring back character sets too).
    let reenters = [&b"\x1b[?47h"[..], b"\x1b[?1047h", b"\x1b[?1049h"]
        .iter()
        .any(|m| case.after.windows(m.len()).any(|w| w == *m));
    if !on_alternate && reenters {
        return Ok(());
    }
    // REP (`CSI n b`) repeats the last character printed, which no restore
    // carries.
    if has_rep(case.after) {
        return Ok(());
    }
    // Reverse wraparound follows the screen rows' soft-wrap marks, which
    // the restore cannot always rebuild (see `restore::differences`).
    if model.mode(45, false) == Some(true) || model.mode(1045, false) == Some(true) {
        return Ok(());
    }
    compare(
        &model,
        &terminal,
        case.mode,
        "after more output",
        on_alternate,
    )
}

/// Whether `bytes` hold a CSI ending in `b` (digits and `;` between).
fn has_rep(bytes: &[u8]) -> bool {
    bytes.windows(2).enumerate().any(|(i, w)| {
        w == b"\x1b[" && {
            let rest = &bytes[i + 2..];
            let n = rest
                .iter()
                .take_while(|c| c.is_ascii_digit() || **c == b';')
                .count();
            rest.get(n) == Some(&b'b')
        }
    })
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
///   cursor. (The saved cursor's version is deferred from M2a.)
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
/// only while the program is on it.
fn compare(
    model: &Model,
    terminal: &Model,
    mode: Mode,
    when: &str,
    alternate: bool,
) -> Result<(), String> {
    let dump = |m: &Model| -> Result<serde_json::Value, String> {
        serde_json::from_slice(&m.dump().map_err(|e| format!("dump: {e:?}"))?)
            .map_err(|e| format!("dump: {e}"))
    };
    let expected = dump(model)?;
    let got = dump(terminal)?;
    let diffs: Vec<String> = restore::differences(&expected, &got, mode)
        .into_iter()
        .filter(|d| !known(&expected, d))
        .filter(|d| alternate || !d.starts_with("alternate"))
        .collect();
    if diffs.is_empty() {
        Ok(())
    } else {
        Err(format!("{when}:\n  {}", diffs.join("\n  ")))
    }
}

/// Differences the restore is known not to cover yet.
fn known(expected: &serde_json::Value, diff: &str) -> bool {
    // A saved cursor waiting to wrap at a right margin inside the screen
    // (DECLRMM) is written without its pending wrap (deferred from M2a).
    let interior_wrap = |screen: &str| {
        let sc = &expected["screens"][screen]["saved_cursor"];
        sc["pending_wrap"] == true
            && sc["x"].as_u64() != Some(expected["cols"].as_u64().unwrap_or(0) - 1)
    };
    if ["primary", "alternate"]
        .iter()
        .any(|s| diff.starts_with(&format!("{s}.saved_cursor")) && interior_wrap(s))
    {
        return true;
    }
    // The blank cell left at the right edge (or right margin) where a wide
    // character did not fit takes the pen in Ghostty; the restore leaves it
    // unstyled.
    let edge = expected["cols"].as_u64().unwrap_or(0);
    let margin = expected["scrolling_region"][3].as_u64().unwrap_or(0) + 1;
    last_column_style_only(diff, edge) || last_column_style_only(diff, margin)
}

/// Whether a row difference (`... row N: [..] != [..]`) is only in the
/// style of the last column and the soft wrap.
fn last_column_style_only(diff: &str, cols: u64) -> bool {
    let Some((_, rest)) = diff.split_once(": ") else {
        return false;
    };
    let Some((a, b)) = rest.split_once(" != ") else {
        return false;
    };
    let (Ok(a), Ok(b)) = (
        serde_json::from_str::<serde_json::Value>(a),
        serde_json::from_str::<serde_json::Value>(b),
    ) else {
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
            b"\x1b[1\"q", b"\x1b[0\"q",
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
        ) {
            // 2027 only from the start: the restore writes it before any
            // content, so text printed before a program turned it on is
            // laid out again with it on (see the PR for M2c).
            let mut stream = if graphemes { b"\x1b[?2027h".to_vec() } else { Vec::new() };
            stream.extend(pieces.concat());
            let after = after.concat();
            let case = Case { stream: &stream, split, model: (cols, rows), client: (cols, rows), mode, after: &after };
            if let Err(e) = check(&case) {
                prop_assert!(false, "{e}\ncase: {case:?}");
            }
        }
    }

    #[test]
    fn fuzz_input_is_read() {
        check_fuzz_input(b"").unwrap();
        check_fuzz_input(b"\x13\x07\x13\x07\x00hello\x1b[31m\xff\xfeworld").unwrap();
    }
}
