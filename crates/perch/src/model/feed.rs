// SPDX-License-Identifier: GPL-3.0-only

//! What the model is fed: the program's output, cut into numbered frames.
//!
//! The supervisor's tracker sees every byte as it is read. The model gets
//! output only up to the last point where the tracker is at ground; an
//! unfinished sequence or UTF-8 character is held back until it completes.
//! So the model is at ground whenever it has caught up, and a snapshot never
//! waits for the program. Frames are also cut at the last byte of every
//! query and every RIS, so a query's frame number names it and the model's
//! DSR 6 reply carries that number.
//!
//! At most [`LONG_SEQUENCE`] bytes of one unfinished sequence are held. A
//! longer APC, PM, SOS or DCS string, or OSC 52, 1337 or 5522, carries
//! images, clipboard data or requests Perch never restores: it is not given
//! to the model at all. Anything else (a CSI can carry line feeds Ghostty
//! acts on) is fed in pieces as it arrives, and the model is marked partial
//! until the next RIS.
//!
//! Pure code: the supervisor pushes what it reads and acts on the steps.

use crate::queries::{Kind, QueryId};
use crate::tracker::{Seen, Tracker, Unfinished};

/// The most bytes of one unfinished sequence held back from the model.
pub const LONG_SEQUENCE: usize = 16 << 20;
/// The most bytes in one frame message; longer runs are split.
pub const MAX_FRAME: usize = 1 << 20;

/// What the supervisor does with a push, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send these bytes to the model as frame `n`.
    Frame { n: u64, bytes: Vec<u8> },
    /// A query ends the frame just sent; [`QueryId`] is its number.
    Query { id: QueryId, kind: Kind },
    /// RIS ends the frame just sent. A partial model is complete again.
    Reset,
    /// A long string sequence of a kind Perch never restores: the model
    /// does not get it.
    Discarded,
    /// A long sequence that can change the screen: the model gets it in
    /// pieces and is partial until the next RIS.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Long {
    /// Dropping a long string until the tracker leaves it.
    Skip,
    /// Feeding a long sequence as it arrives, until the next ground.
    FeedThrough,
}

/// The output side of one session: the tracker and the bytes held back
/// from the model.
#[derive(Debug)]
pub struct Feeder {
    tracker: Tracker,
    limit: usize,
    next_frame: u64,
    /// Output from offset `sent` up to what the tracker has seen.
    held: Vec<u8>,
    /// Bytes before this offset were sent to the model or discarded.
    sent: u64,
    long: Option<Long>,
    /// The first held byte is a raw C1 control that ended a dropped
    /// string; the model gets its 7-bit form.
    c1_first: bool,
}

impl Default for Feeder {
    fn default() -> Self {
        Feeder::new()
    }
}

impl Feeder {
    pub fn new() -> Feeder {
        Feeder::with_limit(LONG_SEQUENCE)
    }

    /// A feeder whose long-sequence limit is `limit` bytes, so tests can
    /// reach it.
    pub fn with_limit(limit: usize) -> Feeder {
        Feeder {
            tracker: Tracker::new(),
            limit,
            next_frame: 1,
            held: Vec::new(),
            sent: 0,
            long: None,
            c1_first: false,
        }
    }

    /// The tracker, for bracketed paste and ground.
    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// Whether the model has been given everything so far and the stream
    /// is between sequences: a point a fresh model can start from.
    pub fn at_ground_point(&self) -> bool {
        self.tracker.at_ground() && self.sent == self.tracker.fed()
    }

    /// Whether a long sequence is being fed through, so the model is not
    /// at ground until it ends.
    pub fn feeding_through(&self) -> bool {
        self.long == Some(Long::FeedThrough)
    }

    /// Output read from the PTY.
    pub fn push(&mut self, mut chunk: &[u8]) -> Vec<Step> {
        // The limit is checked where a held sequence would pass it, not at
        // the end of the read: otherwise a long string that ends later in
        // the same read would reach the model whole.
        let mut steps = Vec::new();
        while !chunk.is_empty() {
            if self.long == Some(Long::Skip) {
                let used = self.skip(chunk, &mut steps);
                chunk = &chunk[used..];
                continue;
            }
            let room = if self.long.is_none() {
                (self.limit + 1).saturating_sub(self.held.len())
            } else {
                self.limit + 1
            };
            let (piece, rest) = chunk.split_at(room.clamp(1, chunk.len()));
            self.push_piece(piece, &mut steps);
            chunk = rest;
        }
        steps
    }

    /// Output that cannot take a held sequence more than one byte past the
    /// limit, outside a string being dropped.
    fn push_piece(&mut self, piece: &[u8], steps: &mut Vec<Step>) {
        let start = self.tracker.fed();
        let events = self.tracker.feed(piece);
        self.held.extend_from_slice(piece);
        for e in events {
            match e.seen {
                Seen::Query(kind) => {
                    let n = self.cut(e.end, steps);
                    steps.push(Step::Query {
                        id: QueryId(n),
                        kind,
                    });
                }
                Seen::Reset => {
                    self.cut(e.end, steps);
                    steps.push(Step::Reset);
                }
                Seen::Paste(_) => {}
            }
        }
        if self.long == Some(Long::FeedThrough) && self.tracker.ground_offset() > start {
            self.long = None;
        }
        let limit = if self.long == Some(Long::FeedThrough) {
            self.tracker.fed()
        } else {
            self.tracker.ground_offset()
        };
        if limit > self.sent {
            self.cut(limit, steps);
        }
        if self.long.is_none() && self.held.len() > self.limit {
            if never_restored(self.tracker.unfinished()) {
                self.long = Some(Long::Skip);
                self.held.clear();
                self.c1_first = false;
                self.sent = self.tracker.fed();
                steps.push(Step::Discarded);
            } else {
                self.long = Some(Long::FeedThrough);
                self.cut(self.tracker.fed(), steps);
                steps.push(Step::Partial);
            }
        }
    }

    /// Drops a long string a byte at a time until the tracker leaves it,
    /// and returns how many bytes of `chunk` that used.
    ///
    /// A BEL, CAN or SUB that ends it is dropped with it. An ESC (of ST, or
    /// of a CSI) or a raw C1 control starts or does something else, so the
    /// model gets it, and whatever follows whole. A raw C1 byte at ground
    /// would be invalid UTF-8 to the model, so it gets the 7-bit form: the
    /// C1 CSI as `ESC [`, DECID as `ESC Z`, ST as `ESC \`.
    fn skip(&mut self, chunk: &[u8], steps: &mut Vec<Step>) -> usize {
        for (i, &b) in chunk.iter().enumerate() {
            let events = self.tracker.feed(&[b]);
            if is_string(self.tracker.unfinished()) {
                debug_assert!(events.is_empty(), "nothing completes inside a string");
                continue;
            }
            self.long = None;
            let c1 = (0x80..=0x9f).contains(&b);
            if self.tracker.at_ground() && !c1 {
                debug_assert!(events.is_empty());
                self.sent = self.tracker.fed();
                return i + 1;
            }
            self.sent = self.tracker.fed() - 1;
            self.held.push(b);
            self.c1_first = c1;
            if self.tracker.at_ground() {
                // A C1 control that acted at once, maybe a query.
                let n = self.cut(self.tracker.fed(), steps);
                for e in events {
                    if let Seen::Query(kind) = e.seen {
                        steps.push(Step::Query {
                            id: QueryId(n),
                            kind,
                        });
                    }
                }
            }
            return i + 1;
        }
        self.sent = self.tracker.fed();
        chunk.len()
    }

    /// Sends held bytes up to offset `end` as frames of at most
    /// [`MAX_FRAME`] bytes, and returns the last frame's number.
    fn cut(&mut self, end: u64, steps: &mut Vec<Step>) -> u64 {
        let len = usize::try_from(end - self.sent).expect("held bytes fit in memory");
        debug_assert!(len > 0 && len <= self.held.len());
        let rest = self.held.split_off(len);
        let mut bytes = std::mem::replace(&mut self.held, rest);
        if std::mem::take(&mut self.c1_first) {
            let c1 = bytes[0];
            bytes.splice(0..1, [0x1b, c1 - 0x40]);
        }
        let mut n = self.next_frame;
        for piece in bytes.chunks(MAX_FRAME) {
            n = self.next_frame;
            self.next_frame += 1;
            steps.push(Step::Frame {
                n,
                bytes: piece.to_vec(),
            });
        }
        self.sent = end;
        n
    }
}

fn is_string(u: Option<Unfinished>) -> bool {
    matches!(
        u,
        Some(Unfinished::Dcs | Unfinished::SosPmApc | Unfinished::Osc { .. })
    )
}

/// The string kinds whose effect Perch never restores.
fn never_restored(u: Option<Unfinished>) -> bool {
    matches!(
        u,
        Some(
            Unfinished::Dcs
                | Unfinished::SosPmApc
                | Unfinished::Osc {
                    number: Some(52 | 1337 | 5522)
                }
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames' bytes, joined, and the other steps in order.
    fn run(f: &mut Feeder, chunks: &[&[u8]]) -> (Vec<u8>, Vec<Step>) {
        let mut fed = Vec::new();
        let mut other = Vec::new();
        for c in chunks {
            for s in f.push(c) {
                match s {
                    Step::Frame { bytes, .. } => fed.extend(bytes),
                    s => other.push(s),
                }
            }
        }
        (fed, other)
    }

    #[test]
    fn holds_an_unfinished_sequence_until_it_completes() {
        let mut f = Feeder::new();
        assert_eq!(
            f.push(b"ab\x1b[3"),
            [Step::Frame {
                n: 1,
                bytes: b"ab".to_vec()
            }]
        );
        assert!(!f.at_ground_point());
        assert_eq!(f.push(b"1"), []);
        assert_eq!(
            f.push(b"mc\xe2\x82"),
            [Step::Frame {
                n: 2,
                bytes: b"\x1b[31mc".to_vec()
            }]
        );
        assert_eq!(
            f.push(b"\xac"),
            [Step::Frame {
                n: 3,
                bytes: "€".as_bytes().to_vec()
            }]
        );
        assert!(f.at_ground_point());
    }

    #[test]
    fn frames_end_at_each_query_and_ris() {
        let mut f = Feeder::new();
        let steps = f.push(b"a\x1b[6nb\x1b[cc\x1bcd\x1b[5");
        assert_eq!(
            steps,
            [
                Step::Frame {
                    n: 1,
                    bytes: b"a\x1b[6n".to_vec()
                },
                Step::Query {
                    id: QueryId(1),
                    kind: Kind::Dsr6
                },
                Step::Frame {
                    n: 2,
                    bytes: b"b\x1b[c".to_vec()
                },
                Step::Query {
                    id: QueryId(2),
                    kind: Kind::Da1
                },
                Step::Frame {
                    n: 3,
                    bytes: b"c\x1bc".to_vec()
                },
                Step::Reset,
                Step::Frame {
                    n: 4,
                    bytes: b"d".to_vec()
                },
            ]
        );
        assert_eq!(
            f.push(b"n"),
            [
                Step::Frame {
                    n: 5,
                    bytes: b"\x1b[5n".to_vec()
                },
                Step::Query {
                    id: QueryId(5),
                    kind: Kind::Dsr5
                },
            ]
        );
    }

    #[test]
    fn long_runs_are_split_and_a_query_names_the_last_piece() {
        let mut f = Feeder::new();
        let mut big = vec![b'x'; MAX_FRAME * 2 + 10];
        big.extend_from_slice(b"\x1b[6n");
        let steps = f.push(&big);
        let ns: Vec<u64> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Frame { n, bytes } => {
                    assert!(bytes.len() <= MAX_FRAME);
                    Some(*n)
                }
                _ => None,
            })
            .collect();
        assert_eq!(ns, [1, 2, 3]);
        assert_eq!(
            steps.last(),
            Some(&Step::Query {
                id: QueryId(3),
                kind: Kind::Dsr6
            })
        );
    }

    #[test]
    fn a_long_clipboard_string_never_reaches_the_model() {
        let mut f = Feeder::with_limit(16);
        let (fed, other) = run(
            &mut f,
            &[
                b"a\x1b]52;c;",
                &[b'Q'; 40],
                &[b'Q'; 40],
                b"QQ\x07b",
                b"\x1bPq",
                &[b'#'; 20],
                b"#\x1b[6n",
            ],
        );
        // The DCS ends at the ESC that starts the CSI; the query survives.
        assert_eq!(fed, b"ab\x1b[6n");
        assert_eq!(
            other,
            [
                Step::Discarded,
                Step::Discarded,
                Step::Query {
                    id: QueryId(3),
                    kind: Kind::Dsr6
                }
            ]
        );
        assert!(f.at_ground_point());
        // ST ends a skipped string: its ESC is kept, which the model
        // ignores as an ESC dispatch.
        let (fed, _) = run(&mut f, &[b"\x1b_G", &[b'z'; 30], b"\x1b\\c"]);
        assert_eq!(fed, b"\x1b\\c");
        // A raw C1 control ending a skipped APC or PM (where it is not
        // data, as in a DCS or OSC) reaches the model in its 7-bit form:
        // here DECID, which Ghostty takes as DA1, and the C1 CSI of a DSR 6.
        let (fed, other) = run(
            &mut f,
            &[b"\x1b_", &[b'z'; 30], b"\x9a\x1b^", &[b'z'; 30], b"\x9b6n"],
        );
        assert_eq!(fed, b"\x1bZ\x1b[6n");
        assert!(matches!(
            other[..],
            [
                Step::Discarded,
                Step::Query {
                    kind: Kind::Da1,
                    ..
                },
                Step::Discarded,
                Step::Query {
                    kind: Kind::Dsr6,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn a_long_csi_is_fed_through_and_marks_the_model_partial() {
        let mut f = Feeder::with_limit(16);
        let mut csi = b"\x1b[".to_vec();
        csi.extend(std::iter::repeat_n(b'1', 20));
        let (fed, other) = run(&mut f, &[b"a", &csi, b"\n11", b"m\x1b[", b"2J"]);
        assert_eq!(other, [Step::Partial]);
        let mut all = b"a".to_vec();
        all.extend(&csi);
        all.extend(b"\n11m\x1b[2J");
        assert_eq!(fed, all);
        assert!(f.at_ground_point());
        // A long OSC of another number is fed through too.
        let (_, other) = run(&mut f, &[b"\x1b]2;", &[b't'; 20], b"\x07"]);
        assert_eq!(other, [Step::Partial]);
    }

    #[test]
    fn under_the_limit_nothing_is_dropped() {
        let mut f = Feeder::with_limit(64);
        let stream = b"\x1b]52;c;QUFB\x07\x1bPq#0\x1b\\x\x1b[38;5;1mred";
        let mut fed = Vec::new();
        for b in stream {
            for s in f.push(&[*b]) {
                if let Step::Frame { bytes, .. } = s {
                    fed.extend(bytes);
                }
            }
        }
        assert_eq!(fed, stream);
    }

    // Review of M2c, S1: a read that crosses the limit.

    #[test]
    fn a_never_restored_string_ending_in_the_crossing_read_never_reaches_the_model() {
        for kind in [&b"\x1b_G"[..], b"\x1bPq", b"\x1b]52;c;", b"\x1b^", b"\x1bX"] {
            for before in 0..=8 {
                // `before` bytes of body first, then a read that passes the
                // limit of 8 and ends the string.
                let mut f = Feeder::with_limit(8);
                let mut first = kind.to_vec();
                first.truncate(8.min(first.len()));
                let head = &kind[first.len()..];
                let mut second = head.to_vec();
                first.extend(std::iter::repeat_n(
                    b'A',
                    before.min(8 - first.len().min(8)),
                ));
                second.extend(std::iter::repeat_n(b'A', 12));
                second.extend(b"\x1b\\after");
                let (fed, other) = run(&mut f, &[b"x", &first, &second]);
                assert_eq!(fed, b"x\x1b\\after", "{kind:?} {before}");
                assert_eq!(other, [Step::Discarded], "{kind:?} {before}");
            }
        }
    }

    #[test]
    fn a_never_restored_string_in_one_read_over_the_limit_never_reaches_the_model() {
        let mut f = Feeder::with_limit(8);
        let (fed, other) = run(&mut f, &[b"x\x1b_GAAAAAAAAAAAAAAAAA\x1b\\y"]);
        assert_eq!(fed, b"x\x1b\\y");
        assert_eq!(other, [Step::Discarded]);
    }

    #[test]
    fn a_screen_sequence_crossing_the_limit_is_fed_whole_and_in_order() {
        // Held at exactly the limit, then a read with a line feed and the
        // final byte: the model gets every byte, in order, at most once.
        let mut f = Feeder::with_limit(8);
        let (fed, _) = run(&mut f, &[b"\x1b[111111", b"\nm\x1b[6nz"]);
        assert_eq!(fed, b"\x1b[111111\nm\x1b[6nz");
        // Over the limit in one read with nothing after: fed through and
        // partial.
        let mut f = Feeder::with_limit(8);
        let (fed, other) = run(&mut f, &[b"\x1b[1111111111"]);
        assert_eq!(fed, b"\x1b[1111111111");
        assert_eq!(other, [Step::Partial]);
    }

    // Review of M2c, S2: what ends a long sequence is handled as usual.

    #[test]
    fn ris_ending_a_long_csi_resets_after_partial() {
        let mut f = Feeder::with_limit(8);
        let (_, other) = run(&mut f, &[b"\x1b[1111111111", b"11\x1bc"]);
        assert_eq!(other, [Step::Partial, Step::Reset]);
        let mut f = Feeder::with_limit(8);
        let (_, other) = run(&mut f, &[b"\x1b[1111111111\x1bc"]);
        assert_eq!(other, [Step::Partial, Step::Reset]);
    }

    #[test]
    fn a_query_right_after_a_long_sequence_is_tagged() {
        for chunks in [
            &[&b"\x1b[1111111111"[..], b"m\x1b[6n"][..],
            &[b"\x1b[1111111111m\x1b[6n"],
            &[b"\x1b_GAAAAAAAAAA", b"\x1b\\\x1b[6n"],
            &[b"\x1b_GAAAAAAAAAA\x1b\\\x1b[6n"],
            &[b"\x1b_GAAAAAAAAAA\x1b[6n"],
        ] {
            let mut f = Feeder::with_limit(8);
            let (_, other) = run(&mut f, chunks);
            assert!(
                matches!(
                    other.last(),
                    Some(Step::Query {
                        kind: Kind::Dsr6,
                        ..
                    })
                ),
                "{chunks:?}: {other:?}"
            );
        }
    }

    #[test]
    fn a_long_string_after_another_long_sequence_in_the_same_read_is_dropped() {
        let apc = b"\x1b_GAAAAAAAAAAAA\x1b\\";
        // After a CSI fed through.
        let mut f = Feeder::with_limit(8);
        let mut read = b"\x1b[1111111111m".to_vec();
        read.extend(apc);
        read.extend(b"z");
        let (fed, other) = run(&mut f, &[&read]);
        assert_eq!(fed, b"\x1b[1111111111m\x1b\\z");
        assert_eq!(other, [Step::Partial, Step::Discarded]);
        // After a string dropped.
        let mut f = Feeder::with_limit(8);
        let mut read = apc.to_vec();
        read.extend(apc);
        let (fed, other) = run(&mut f, &[&read]);
        assert_eq!(fed, b"\x1b\\\x1b\\");
        assert_eq!(other, [Step::Discarded, Step::Discarded]);
    }
}
