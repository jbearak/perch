// SPDX-License-Identifier: GPL-3.0-only

//! The tracker against `perch-vt`: the same bytes, in the same chunks, to
//! both, and after every chunk the same ground, the same queries and
//! bracketed-paste changes at the same offsets in the same order, and the
//! same bracketed-paste state. `perch-vt`'s test observer is the oracle, so
//! this exists only with `test-hooks`. The proptest below and the `tracker`
//! fuzz target both run it.

use crate::queries::Kind;
use crate::tracker::{Event, PasteOp, Seen, Tracker};
use crate::vt_ffi::{Model, Observed};

/// Feeds `stream` to a tracker and a model in chunks of the given sizes,
/// cycled (a zero counts as one), and describes the first disagreement.
pub fn check(stream: &[u8], sizes: &[usize]) -> Result<(), String> {
    let mut tracker = Tracker::new();
    let mut model = Model::new(20, 5, 100, 1 << 20).map_err(|e| format!("model: {e:?}"))?;
    let mut sizes = sizes.iter().map(|n| (*n).max(1)).cycle();
    let mut rest = stream;
    let mut frame = 0;
    while !rest.is_empty() {
        let n = sizes.next().unwrap_or(rest.len()).min(rest.len());
        let (chunk, tail) = rest.split_at(n);
        rest = tail;
        let ours: Vec<(u32, u64)> = tracker.feed(chunk).iter().filter_map(observed).collect();
        model.feed(chunk, frame);
        let theirs: Vec<(u32, u64)> = model
            .take_observed()
            .iter()
            .map(|o| (o.kind, o.end))
            .collect();
        let at = tracker.fed();
        if ours != theirs {
            return Err(format!(
                "chunk {frame} ending at {at}: tracker saw {ours:?}, perch-vt saw {theirs:?}"
            ));
        }
        if tracker.at_ground() != model.at_ground() {
            return Err(format!(
                "chunk {frame} ending at {at}: tracker at ground {}, perch-vt {}",
                tracker.at_ground(),
                model.at_ground()
            ));
        }
        let paste = model.mode(2004, false) == Some(true);
        if tracker.bracketed_paste() != paste {
            return Err(format!(
                "chunk {frame} ending at {at}: tracker bracketed paste {}, perch-vt {paste}",
                tracker.bracketed_paste()
            ));
        }
        frame += 1;
    }
    Ok(())
}

/// A tracker event as the observer records it. The observer does not
/// record RIS; its effect shows in the bracketed-paste state.
fn observed(event: &Event) -> Option<(u32, u64)> {
    let kind = match event.seen {
        Seen::Query(Kind::Dsr5) => Observed::DSR5,
        Seen::Query(Kind::Dsr6) => Observed::DSR6,
        Seen::Query(Kind::Da1) => Observed::DA1,
        Seen::Query(Kind::Da2) => Observed::DA2,
        Seen::Paste(PasteOp::Set) => Observed::PASTE_SET,
        Seen::Paste(PasteOp::Reset) => Observed::PASTE_RESET,
        Seen::Paste(PasteOp::Save) => Observed::PASTE_SAVE,
        Seen::Paste(PasteOp::Restore) => Observed::PASTE_RESTORE,
        Seen::Reset => return None,
    };
    Some((kind, event.end))
}

/// Splits fuzz input into chunk sizes and a stream: the first byte says
/// how many of the next bytes are sizes (at most 15), each 1 to 64.
pub fn check_fuzz_input(data: &[u8]) -> Result<(), String> {
    let Some((&first, rest)) = data.split_first() else {
        return Ok(());
    };
    let n = usize::from(first % 16).min(rest.len());
    let (sizes, stream) = rest.split_at(n);
    let sizes: Vec<usize> = if sizes.is_empty() {
        vec![stream.len().max(1)]
    } else {
        sizes.iter().map(|b| usize::from(b % 64) + 1).collect()
    };
    check(stream, &sizes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Pieces that make the interesting transitions likely: introducers,
    /// terminators, aborts, C1 bytes raw and as UTF-8, split UTF-8, the
    /// tracked sequences whole and in parts, and any byte at all.
    fn piece() -> impl Strategy<Value = Vec<u8>> {
        #[rustfmt::skip]
        const PIECES: &[&[u8]] = &[
            b"\x1b", b"[", b"]", b"P", b"X", b"^", b"_", b"\\", b"?", b">", b"=", b"!", b" ",
            b"#", b";", b":", b"0", b"1", b"5", b"6", b"2004", b"n", b"c", b"h", b"l", b"s",
            b"r", b"p", b"Z", b"m", b"\x07", b"\x18", b"\x1a", b"\n", b"\r", b"\x7f", b"\x9a",
            b"\x9b", b"\x9c", b"\x9d", b"\x90", b"\x98", b"\xc2\x9b", b"\xc2", b"\xe2\x82",
            b"\xe2\x82\xac", b"\xf0\x9f\x98", b"\xf0\x9f\x98\x80", b"x", b"\x1b[5n",
            b"\x1b[6n", b"\x1b[c", b"\x1b[>c", b"\x1b[?2004h", b"\x1b[?2004l",
            b"\x1b[?2004s", b"\x1b[?2004r", b"\x1b[?1000;2004h", b"\x1bc", b"\x1b[!p",
            b"\x1b]52;c;", b"\x1bPq", b"\x1b_G",
        ];
        prop_oneof![
            4 => proptest::sample::select(PIECES).prop_map(<[u8]>::to_vec),
            1 => any::<u8>().prop_map(|b| vec![b]),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        #[test]
        fn tracker_agrees_with_perch_vt(
            pieces in proptest::collection::vec(piece(), 0..120),
            sizes in proptest::collection::vec(1usize..48, 1..12),
        ) {
            let stream = pieces.concat();
            if let Err(e) = check(&stream, &sizes) {
                prop_assert!(false, "{e}\nstream: {stream:?}\nsizes: {sizes:?}");
            }
        }
    }

    #[test]
    fn known_streams_agree() {
        for stream in [
            &b"ab\x1b[5n\x1b[6n\x1b[c\x1b[>c\x1bZ\x1b[12\x9a"[..],
            b"\xc2\x9b6n\x1b\x9b6n\x9b5n",
            b"\x1b[?2004h\x1b[?2004s\x1b[?2004l\x1b[?2004r\x1bc\x1b[?2004r",
            b"\x1b]0;t\x18\x1b[6n\x1b]52;c;x\x1b[5n\x1bPq\x9c\x1b\\",
            b"\x1b[\n6\r\x08n\x1b[6\x18n\x1b[?20\n04h",
            "aé€😀".as_bytes(),
        ] {
            for size in 1..=stream.len() {
                check(stream, &[size]).unwrap();
            }
        }
        check_fuzz_input(b"\x03\x01\x02\x03\x1b[6n").unwrap();
        check_fuzz_input(b"").unwrap();
    }
}
