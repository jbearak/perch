// SPDX-License-Identifier: GPL-3.0-only

//! The session's input queue: what the supervisor writes to the PTY master.
//!
//! One FIFO holds the attached client's keystrokes and `send` requests, each
//! as a whole item, so a send's bytes are never interleaved with typed ones.
//! Replies to terminal queries go ahead of every item not yet started, but
//! never into the middle of one already partly written. They come from the
//! supervisor ([`Input::push_reply`]) or from client input recognized as the
//! reply to an outstanding forwarded query ([`Input::client_read`]).
//!
//! Pure code: the caller passes the time as a [`Duration`] from any fixed
//! origin and writes through a [`Transport`].

use std::collections::VecDeque;
use std::time::Duration;

use crate::queries::{Kind, ReplyMatcher};
use crate::tracker::reply_kind;
use crate::transport::Transport;

/// The most bytes the queue holds. Replies are taken even over it.
pub const QUEUE_CAP: usize = 1 << 20;
/// The most replies queued. A program that keeps asking while it reads no
/// input gets no more answers past this, so the queue stays bounded.
pub const REPLY_MAX: usize = 4096;
/// How long an incomplete escape sequence at the end of a client read is
/// held while a forwarded query is outstanding.
pub const REPLY_HOLD: Duration = Duration::from_millis(100);
/// The longest sequence held. Every reply shape is far shorter.
const HOLD_MAX: usize = 256;

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// A send did not fit; nothing was queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull;

#[derive(Debug)]
struct Held {
    bytes: Vec<u8>,
    deadline: Duration,
}

/// One session's input queue.
#[derive(Debug, Default)]
pub struct Input {
    replies: VecDeque<Vec<u8>>,
    items: VecDeque<Vec<u8>>,
    /// The item or reply partly written, and how much of it.
    current: Option<(Vec<u8>, usize)>,
    queued: usize,
    held: Option<Held>,
    /// An incomplete escape sequence that ended the last client read and
    /// was queued in order, because nothing was outstanding, the hold
    /// expired, or it was too long. A reply it begins is still the
    /// client's answer when a later read completes it, so it is still
    /// matched; it just gets no priority. Without that, the supervisor
    /// would answer the same query again on detach.
    tail: Vec<u8>,
}

impl Input {
    pub fn new() -> Input {
        Input::default()
    }

    /// Queues a `send`, whole or not at all. With `paste`, the data is
    /// wrapped in bracketed-paste markers only if `bracketed_paste` (the
    /// tracker's state) says the program turned them on. With `submit`, a
    /// carriage return follows.
    pub fn send(
        &mut self,
        data: &[u8],
        paste: bool,
        submit: bool,
        bracketed_paste: bool,
    ) -> Result<(), QueueFull> {
        let wrap = paste && bracketed_paste;
        let mut item = Vec::new();
        if wrap {
            item.extend_from_slice(PASTE_START);
        }
        item.extend_from_slice(data);
        if wrap {
            item.extend_from_slice(PASTE_END);
        }
        if submit {
            item.push(b'\r');
        }
        if item.len() > self.room() {
            return Err(QueueFull);
        }
        self.push_item(item);
        Ok(())
    }

    /// Queues a reply ahead of every item not yet started, unless
    /// [`REPLY_MAX`] replies are already waiting.
    pub fn push_reply(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() && self.replies.len() < REPLY_MAX {
            self.queued += bytes.len();
            self.replies.push_back(bytes);
        }
    }

    /// Bytes the attached client sent, in one read of at most
    /// [`Input::room`] bytes. They pass to the program unchanged, in order,
    /// except that a complete reply to an outstanding forwarded query goes
    /// ahead as a reply. While a forwarded query is outstanding, an
    /// incomplete escape sequence at the end is held until a later read
    /// completes it or [`REPLY_HOLD`] passes.
    pub fn client_read(&mut self, bytes: &[u8], now: Duration, matcher: &mut impl ReplyMatcher) {
        // Bytes before `queued` went in order with an earlier read.
        let (mut buf, queued, held_since) = match self.held.take() {
            Some(held) => (held.bytes, 0, Some(held.deadline)),
            None => {
                let tail = std::mem::take(&mut self.tail);
                let n = tail.len();
                (tail, n, None)
            }
        };
        buf.extend_from_slice(bytes);
        let mut start = queued;
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != ESC {
                i += 1;
                continue;
            }
            match scan(&buf[i..]) {
                Scan::Complete(n, kind) => {
                    if let Some(kind) = kind {
                        if i < queued {
                            // Its start is already queued: count the reply,
                            // and let the rest follow in order.
                            matcher.match_reply(kind);
                        } else if matcher.match_reply(kind) {
                            self.push_typed(&buf[start..i]);
                            self.push_reply(buf[i..i + n].to_vec());
                            start = i + n;
                        }
                    }
                    i += n;
                }
                Scan::Incomplete => {
                    let rest = buf.len() - i;
                    if i >= queued && rest <= HOLD_MAX && matcher.outstanding() {
                        self.push_typed(&buf[start..i]);
                        // A sequence still growing from an earlier read keeps
                        // that read's deadline.
                        let deadline = match held_since {
                            Some(d) if i == 0 => d,
                            _ => now + REPLY_HOLD,
                        };
                        self.held = Some(Held {
                            bytes: buf[i..].to_vec(),
                            deadline,
                        });
                        return;
                    }
                    if rest <= HOLD_MAX {
                        self.tail = buf[i..].to_vec();
                    }
                    break;
                }
            }
        }
        self.push_typed(&buf[start..]);
    }

    /// The client is leaving: nothing more will complete a held sequence,
    /// so it is queued in order.
    pub fn client_left(&mut self) {
        if let Some(held) = self.held.take() {
            self.push_typed(&held.bytes);
        }
        self.tail.clear();
    }

    /// Queues a held sequence whose time is up, in order.
    pub fn tick(&mut self, now: Duration) {
        if self.held.as_ref().is_some_and(|h| now >= h.deadline) {
            let held = self.held.take().expect("checked");
            self.push_typed(&held.bytes);
            self.tail = held.bytes;
        }
    }

    /// When [`Input::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.held.as_ref().map(|h| h.deadline)
    }

    /// Bytes a send or a client read may still add.
    pub fn room(&self) -> usize {
        QUEUE_CAP.saturating_sub(self.queued)
    }

    /// Bytes queued and not yet written.
    pub fn queued(&self) -> usize {
        self.queued
    }

    /// The bytes to write next: the rest of a partly written item, else the
    /// oldest reply, else the oldest item.
    pub fn pending(&self) -> Option<&[u8]> {
        if let Some((item, off)) = &self.current {
            return Some(&item[*off..]);
        }
        self.replies
            .front()
            .or(self.items.front())
            .map(Vec::as_slice)
    }

    /// The PTY took `n` bytes of [`Input::pending`].
    pub fn wrote(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let (item, off) = self.current.get_or_insert_with(|| {
            let next = self.replies.pop_front().or_else(|| self.items.pop_front());
            (next.expect("wrote with nothing pending"), 0)
        });
        assert!(*off + n <= item.len(), "wrote more than was pending");
        *off += n;
        self.queued -= n;
        if *off == item.len() {
            self.current = None;
        }
    }

    /// Writes as much as `pty` accepts and returns how much that was.
    pub fn flush(&mut self, pty: &mut impl Transport) -> usize {
        let mut total = 0;
        while let Some(bytes) = self.pending() {
            let n = pty.write(bytes);
            if n == 0 {
                break;
            }
            self.wrote(n);
            total += n;
        }
        total
    }

    fn push_typed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.push_item(bytes.to_vec());
        }
    }

    fn push_item(&mut self, item: Vec<u8>) {
        if !item.is_empty() {
            self.queued += item.len();
            self.items.push_back(item);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Scan {
    /// A sequence of this many bytes, and the reply kind its shape matches.
    Complete(usize, Option<Kind>),
    /// The bytes run out before the sequence ends.
    Incomplete,
}

/// Scans the escape sequence at the start of `b`, which begins with ESC.
/// Recognizes CSI, SS3 and two-byte escapes, which is all a terminal's
/// keys and replies to the four queries use. A byte that cannot continue a
/// sequence ends it before that byte.
fn scan(b: &[u8]) -> Scan {
    debug_assert_eq!(b.first(), Some(&ESC));
    let mut i = 1;
    // Intermediates of a plain escape (`ESC ( B` and the like).
    while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
        i += 1;
    }
    let Some(&c) = b.get(i) else {
        return Scan::Incomplete;
    };
    if i > 1 || (c != b'[' && c != b'O') {
        return Scan::Complete(if (0x30..=0x7e).contains(&c) { i + 1 } else { i }, None);
    }
    if c == b'O' {
        return match b.get(2) {
            None => Scan::Incomplete,
            Some(0x20..=0x7e) => Scan::Complete(3, None),
            Some(_) => Scan::Complete(2, None),
        };
    }
    let params_start = 2;
    let mut i = params_start;
    while i < b.len() && (0x30..=0x3f).contains(&b[i]) {
        i += 1;
    }
    let params = &b[params_start..i];
    let inter_start = i;
    while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
        i += 1;
    }
    let Some(&fin) = b.get(i) else {
        return Scan::Incomplete;
    };
    if !(0x40..=0x7e).contains(&fin) {
        return Scan::Complete(i, None);
    }
    let kind = if i > inter_start {
        None
    } else {
        reply_kind(params, fin)
    };
    Scan::Complete(i + 1, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Scripted;

    /// Outstanding forwarded queries, by kind, oldest first.
    #[derive(Default)]
    struct Outstanding(Vec<Kind>);

    impl ReplyMatcher for Outstanding {
        fn outstanding(&self) -> bool {
            !self.0.is_empty()
        }
        fn match_reply(&mut self, kind: Kind) -> bool {
            match self.0.iter().position(|k| *k == kind) {
                Some(i) => {
                    self.0.remove(i);
                    true
                }
                None => false,
            }
        }
    }

    const MS: Duration = Duration::from_millis(1);

    fn drain(input: &mut Input) -> Vec<u8> {
        let mut pty = Scripted::new();
        pty.open();
        input.flush(&mut pty);
        pty.take()
    }

    #[test]
    fn sends_and_keystrokes_keep_fifo_order() {
        let mut input = Input::new();
        let mut none = Outstanding::default();
        input.client_read(b"ab", MS, &mut none);
        input.send(b"print(1)", false, true, false).unwrap();
        input.client_read(b"cd", MS, &mut none);
        assert_eq!(drain(&mut input), b"abprint(1)\rcd");
        assert_eq!(input.queued(), 0);
    }

    #[test]
    fn paste_wraps_only_when_the_program_turned_it_on() {
        let mut input = Input::new();
        input.send(b"x <- 1", true, false, true).unwrap();
        assert_eq!(drain(&mut input), b"\x1b[200~x <- 1\x1b[201~");
        input.send(b"x <- 1", true, true, false).unwrap();
        assert_eq!(drain(&mut input), b"x <- 1\r");
        input.send(b"y", true, true, true).unwrap();
        assert_eq!(drain(&mut input), b"\x1b[200~y\x1b[201~\r");
        input.send(b"", false, true, false).unwrap();
        assert_eq!(drain(&mut input), b"\r");
    }

    #[test]
    fn queue_full_is_atomic() {
        let mut input = Input::new();
        input
            .send(&vec![b'a'; QUEUE_CAP - 10], false, false, false)
            .unwrap();
        assert_eq!(input.send(&[b'b'; 10], false, true, false), Err(QueueFull));
        assert_eq!(input.queued(), QUEUE_CAP - 10);
        // The same send fits exactly once the submit byte is not added.
        input.send(&[b'b'; 10], false, false, false).unwrap();
        assert_eq!(input.room(), 0);
        assert_eq!(input.send(b"c", false, false, false), Err(QueueFull));
        // Replies are always taken.
        input.push_reply(b"\x1b[0n".to_vec());
        let out = drain(&mut input);
        assert_eq!(&out[..4], b"\x1b[0n");
        assert_eq!(out.len(), QUEUE_CAP + 4);
        assert_eq!(input.room(), QUEUE_CAP);
    }

    #[test]
    fn replies_stop_at_the_reply_limit() {
        let mut input = Input::new();
        for _ in 0..REPLY_MAX + 10 {
            input.push_reply(b"\x1b[0n".to_vec());
        }
        assert_eq!(input.queued(), REPLY_MAX * 4);
    }

    #[test]
    fn a_reply_goes_ahead_of_unstarted_items() {
        let mut input = Input::new();
        input.send(b"one", false, false, false).unwrap();
        input.send(b"two", false, false, false).unwrap();
        input.push_reply(b"\x1b[3;4R".to_vec());
        assert_eq!(drain(&mut input), b"\x1b[3;4Ronetwo");
    }

    #[test]
    fn a_reply_never_splits_a_partly_written_send() {
        let mut input = Input::new();
        input.send(b"hello world", false, true, false).unwrap();
        input.send(b"next", false, false, false).unwrap();
        let mut pty = Scripted::new();
        pty.allow(5);
        assert_eq!(input.flush(&mut pty), 5);
        assert_eq!(pty.out, b"hello");
        input.push_reply(b"\x1b[0n".to_vec());
        pty.open();
        input.flush(&mut pty);
        assert_eq!(pty.take(), b"hello world\r\x1b[0nnext");
    }

    #[test]
    fn a_reply_waits_for_a_partly_written_reply() {
        let mut input = Input::new();
        input.push_reply(b"\x1b[?1;2c".to_vec());
        let mut pty = Scripted::new();
        pty.allow(3);
        input.flush(&mut pty);
        input.push_reply(b"\x1b[0n".to_vec());
        input.send(b"x", false, false, false).unwrap();
        pty.open();
        input.flush(&mut pty);
        assert_eq!(pty.take(), b"\x1b[?1;2c\x1b[0nx");
    }

    #[test]
    fn a_zero_byte_write_starts_nothing() {
        let mut input = Input::new();
        input.send(b"big send", false, false, false).unwrap();
        let mut pty = Scripted::new();
        assert_eq!(input.flush(&mut pty), 0);
        input.push_reply(b"\x1b[0n".to_vec());
        pty.open();
        input.flush(&mut pty);
        assert_eq!(pty.take(), b"\x1b[0nbig send");
    }

    #[test]
    fn a_client_reply_to_an_outstanding_query_jumps_the_queue() {
        let mut input = Input::new();
        let mut q = Outstanding(vec![Kind::Dsr6]);
        input.send(b"large send", false, false, false).unwrap();
        input.client_read(b"ab\x1b[12;5Rcd", MS, &mut q);
        assert!(q.0.is_empty());
        assert_eq!(drain(&mut input), b"\x1b[12;5Rlarge sendabcd");
    }

    #[test]
    fn reply_shapes_without_an_outstanding_query_stay_in_order() {
        let mut input = Input::new();
        // Shift+F3 in some terminals, and a DA1 reply nobody asked for.
        let mut q = Outstanding(vec![Kind::Dsr5]);
        input.send(b"s", false, false, false).unwrap();
        input.client_read(b"\x1b[1;2R\x1b[?1;2c", MS, &mut q);
        assert_eq!(q.0, [Kind::Dsr5]);
        assert_eq!(drain(&mut input), b"s\x1b[1;2R\x1b[?1;2c");
    }

    #[test]
    fn each_reply_shape_matches_its_kind() {
        for (bytes, kind) in [
            (&b"\x1b[0n"[..], Kind::Dsr5),
            (b"\x1b[24;80R", Kind::Dsr6),
            (b"\x1b[?62;22c", Kind::Da1),
            (b"\x1b[?1;2c", Kind::Da1),
            (b"\x1b[>0;0;0c", Kind::Da2),
            (b"\x1b[>c", Kind::Da2),
        ] {
            assert_eq!(scan(bytes), Scan::Complete(bytes.len(), Some(kind)));
        }
        for bytes in [
            &b"\x1b[5n"[..],
            b"\x1b[R",
            b"\x1b[1R",
            b"\x1b[1;2;3R",
            b"\x1b[;5R",
            b"\x1b[?6;1R",
            b"\x1b[c",
            b"\x1b[1;2 R",
            b"\x1b[A",
            b"\x1bOP",
        ] {
            assert_eq!(scan(bytes), Scan::Complete(bytes.len(), None), "{bytes:?}");
        }
    }

    #[test]
    fn a_reply_split_across_reads_is_held_then_queued_at_the_front() {
        let mut input = Input::new();
        let mut q = Outstanding(vec![Kind::Dsr6]);
        input.send(b"queued", false, false, false).unwrap();
        input.client_read(b"ab\x1b[12;", 10 * MS, &mut q);
        assert_eq!(input.next_deadline(), Some(110 * MS));
        input.client_read(b"5", 50 * MS, &mut q);
        // Still growing from the first read: the deadline does not move.
        assert_eq!(input.next_deadline(), Some(110 * MS));
        input.client_read(b"Rcd", 109 * MS, &mut q);
        assert_eq!(input.next_deadline(), None);
        assert!(q.0.is_empty());
        assert_eq!(drain(&mut input), b"\x1b[12;5Rqueuedabcd");
    }

    #[test]
    fn a_held_sequence_goes_in_order_after_100_ms() {
        let mut input = Input::new();
        let mut q = Outstanding(vec![Kind::Dsr6]);
        input.client_read(b"a\x1b[1", Duration::ZERO, &mut q);
        input.send(b"s", false, false, false).unwrap();
        input.tick(99 * MS);
        assert_eq!(drain(&mut input), b"as");
        input.tick(100 * MS);
        // The rest arrives too late to go ahead of anything, but it is still
        // the client's reply, so the query is no longer outstanding.
        input.send(b"t", false, false, false).unwrap();
        input.client_read(b";1R", 150 * MS, &mut q);
        assert_eq!(drain(&mut input), b"\x1b[1t;1R");
        assert!(q.0.is_empty());
    }

    #[test]
    fn a_held_sequence_that_completes_as_a_key_goes_in_order() {
        let mut input = Input::new();
        let mut q = Outstanding(vec![Kind::Da1]);
        input.send(b"s", false, false, false).unwrap();
        input.client_read(b"\x1b[", Duration::ZERO, &mut q);
        input.client_read(b"Ax", 20 * MS, &mut q);
        assert_eq!(drain(&mut input), b"s\x1b[Ax");
        // A lone ESC (the Escape key) is held and released the same way.
        input.client_read(b"\x1b", 30 * MS, &mut q);
        assert_eq!(drain(&mut input), b"");
        input.tick(130 * MS);
        assert_eq!(drain(&mut input), b"\x1b");
    }

    #[test]
    fn nothing_is_held_without_an_outstanding_query() {
        let mut input = Input::new();
        let mut none = Outstanding::default();
        input.client_read(b"a\x1b[12;", Duration::ZERO, &mut none);
        assert_eq!(input.next_deadline(), None);
        assert_eq!(drain(&mut input), b"a\x1b[12;");
        // Nor is anything matched when the sequence completes.
        input.client_read(b"5R", MS, &mut none);
        assert_eq!(drain(&mut input), b"5R");
    }

    #[test]
    fn a_sequence_completed_across_reads_after_release_still_counts() {
        let mut input = Input::new();
        let mut q = Outstanding::default();
        // Nothing outstanding when the first half arrives.
        input.client_read(b"\x1b[?6", Duration::ZERO, &mut q);
        q.0.push(Kind::Da1);
        input.client_read(b"2;", MS, &mut q);
        assert_eq!(q.0, [Kind::Da1], "still incomplete");
        input.client_read(b"22cz", 2 * MS, &mut q);
        assert!(q.0.is_empty());
        assert_eq!(drain(&mut input), b"\x1b[?62;22cz");
        // A key that completes it is not a reply.
        q.0.push(Kind::Dsr6);
        input.client_read(b"\x1b", 3 * MS, &mut q);
        input.tick(103 * MS);
        input.client_read(b"[A", 104 * MS, &mut q);
        assert_eq!(q.0, [Kind::Dsr6]);
        assert_eq!(drain(&mut input), b"\x1b[A");
    }

    #[test]
    fn a_departing_client_releases_what_it_held() {
        let mut input = Input::new();
        let mut q = Outstanding(vec![Kind::Dsr6]);
        input.client_read(b"\x1b[3", Duration::ZERO, &mut q);
        input.client_left();
        assert_eq!(drain(&mut input), b"\x1b[3");
    }
}
