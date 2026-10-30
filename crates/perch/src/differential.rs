// SPDX-License-Identifier: GPL-3.0-only

//! The tracker against `perch-vt`: the same bytes, in the same chunks, to
//! both, and after every chunk the same ground, the same queries and
//! bracketed-paste changes at the same offsets in the same order, and the
//! same bracketed-paste state. `perch-vt`'s test observer is the oracle, so
//! this exists only with `test-hooks`. The proptest below and the `tracker`
//! fuzz target both run it.
//!
//! The observer feeds Ghostty a byte at a time, so that each query knows
//! its offset. The model process feeds it through Ghostty's bulk path
//! instead, in the frames [`Feeder`] cuts. [`check_production`] runs that
//! path too and holds it to the same tracker and to the observer.

use std::collections::HashMap;

use crate::model::feed::{Feeder, Step};
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

/// Feeds `stream` in chunks of the given sizes through the production
/// path: the supervisor's [`Feeder`] (held back to ground, cut at queries,
/// with a long-sequence limit of `limit` bytes) into a model fed with
/// Ghostty's bulk path, as the model process does. Alongside, a second
/// model gets the same frames a byte at a time, with the observer. After
/// every chunk:
///
/// - the bulk model is at ground (it is fed only up to ground), unless a
///   long sequence is being fed through, when it agrees with the tracker;
/// - its bracketed paste is the tracker's;
/// - its DSR 6 replies are the observer model's, one per DSR 6 query the
///   feeder reported, tagged with that query's frame;
/// - the observer saw exactly the queries the feeder reported, each ending
///   its frame.
pub fn check_production(stream: &[u8], sizes: &[usize], limit: usize) -> Result<(), String> {
    let mut feeder = Feeder::with_limit(limit);
    let mut bulk = Model::new(20, 5, 100, 1 << 20).map_err(|e| format!("model: {e:?}"))?;
    let mut bytewise = Model::new(20, 5, 100, 1 << 20).map_err(|e| format!("model: {e:?}"))?;
    let mut fed = 0u64;
    let mut frame_end = HashMap::new();
    let mut sizes = sizes.iter().map(|n| (*n).max(1)).cycle();
    let mut rest = stream;
    let mut chunk_no = 0;
    while !rest.is_empty() {
        let n = sizes.next().unwrap_or(rest.len()).min(rest.len());
        let (chunk, tail) = rest.split_at(n);
        rest = tail;
        let mut queries = Vec::new();
        let mut dsr6 = Vec::new();
        for step in feeder.push(chunk) {
            match step {
                Step::Frame { n, bytes } => {
                    bulk.feed_slice(&bytes, n);
                    bytewise.feed(&bytes, n);
                    fed += bytes.len() as u64;
                    frame_end.insert(n, fed);
                }
                Step::Query { id, kind, .. } => {
                    queries.push((query_kind(kind), frame_end[&id.0]));
                    if kind == Kind::Dsr6 {
                        dsr6.push(id.0);
                    }
                }
                Step::Reset | Step::Discarded | Step::Partial | Step::LongEnd { .. } => {}
            }
        }
        let at = feeder.tracker().fed();
        let fail = |what: String| Err(format!("chunk {chunk_no} ending at {at}: {what}"));
        bulk.take_observed();
        let seen: Vec<(u32, u64)> = bytewise
            .take_observed()
            .iter()
            .filter(|o| o.kind <= Observed::DA2)
            .map(|o| (o.kind, o.end))
            .collect();
        if seen != queries {
            return fail(format!(
                "feeder reported {queries:?}, observer saw {seen:?}"
            ));
        }
        let replies = bulk.take_replies();
        let expected = bytewise.take_replies();
        if replies != expected {
            return fail(format!(
                "bulk path replied {replies:?}, byte path {expected:?}"
            ));
        }
        let frames: Vec<u64> = replies.iter().map(|r| r.frame).collect();
        if frames != dsr6 {
            return fail(format!("replies for frames {frames:?}, DSR 6 in {dsr6:?}"));
        }
        let ground = if feeder.feeding_through() {
            feeder.tracker().at_ground()
        } else {
            true
        };
        if bulk.at_ground() != ground || bytewise.at_ground() != ground {
            return fail(format!(
                "bulk model at ground {}, byte model {}, expected {ground}",
                bulk.at_ground(),
                bytewise.at_ground()
            ));
        }
        let paste = bulk.mode(2004, false) == Some(true);
        if paste != feeder.tracker().bracketed_paste() {
            return fail(format!(
                "bulk model bracketed paste {paste}, tracker {}",
                feeder.tracker().bracketed_paste()
            ));
        }
        chunk_no += 1;
    }
    Ok(())
}

fn query_kind(kind: Kind) -> u32 {
    match kind {
        Kind::Dsr5 => Observed::DSR5,
        Kind::Dsr6 => Observed::DSR6,
        Kind::Da1 => Observed::DA1,
        Kind::Da2 => Observed::DA2,
    }
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
    check(stream, &sizes)?;
    // The production path, with a long-sequence limit small enough for
    // fuzz inputs to pass it.
    check_production(stream, &sizes, usize::from(first / 16) * 8 + 8)
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

    /// Pieces for the production path: the above, and long strings and
    /// sequences that pass a small long-sequence limit.
    fn long_piece() -> impl Strategy<Value = Vec<u8>> {
        #[rustfmt::skip]
        const INTRODUCERS: &[&[u8]] = &[
            b"\x1b]52;c;", b"\x1b]1337;", b"\x1b]2;", b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX",
            b"\x1b[", b"\x1b[?", b"\x1b]",
        ];
        prop_oneof![
            8 => piece(),
            1 => (proptest::sample::select(INTRODUCERS), 1usize..80, any::<bool>()).prop_map(
                |(intro, n, digits)| {
                    let mut v = intro.to_vec();
                    v.extend(std::iter::repeat_n(if digits { b'1' } else { b'x' }, n));
                    v
                }
            ),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        #[test]
        fn production_feed_agrees_with_tracker_and_observer(
            pieces in proptest::collection::vec(long_piece(), 0..120),
            sizes in proptest::collection::vec(1usize..48, 1..12),
            limit in prop_oneof![1 => Just(crate::model::feed::LONG_SEQUENCE), 2 => 4usize..64],
        ) {
            let stream = pieces.concat();
            if let Err(e) = check_production(&stream, &sizes, limit) {
                prop_assert!(false, "{e}\nstream: {stream:?}\nsizes: {sizes:?}\nlimit: {limit}");
            }
        }

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
        for stream in [
            &b"ab\x1b[6n\x1b[5n\x1b[?2004h\x1bc\x1b[6n"[..],
            b"\x1b]52;c;QUFBQUFBQUFBQUFB\x07\x1b[6n",
            b"\x1bPqqqqqqqqqqqqqqqq\x1b[?2004h\x1b[6n",
            b"\x1b[1111111111111111111\n1m\x1b[6n",
        ] {
            for size in 1..=stream.len() {
                for limit in [4, 8, crate::model::feed::LONG_SEQUENCE] {
                    check_production(stream, &[size], limit).unwrap();
                }
            }
        }
        check_fuzz_input(b"").unwrap();
    }
}
