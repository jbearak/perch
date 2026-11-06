// SPDX-License-Identifier: GPL-3.0-only

//! Scripted scenarios through the supervisor's [`Exchange`], with exact bytes
//! at the client's socket and at the PTY, a scripted model and client
//! terminal; and property tests that no scenario gives two replies to one
//! query.

use std::collections::BTreeMap;
use std::time::Duration;

use proptest::prelude::*;

use crate::exchange::{ClientIo, Exchange, Need};
use crate::flow::{self, ClientFlow, Phase};
use crate::input::{Input, QueueFull};
use crate::queries::{self, Kind, ModelState, NoReply, QueryId};
use crate::transport::Scripted;

const MS: Duration = Duration::from_millis(1);
const S: Duration = Duration::from_secs(1);
/// An output frame's length and type, ahead of its payload.
const HEADER: usize = 5;

/// The attached client, as the scenario scripts it.
struct Client {
    /// Supervisor to client.
    sock: Scripted,
    /// What the client has made of its socket's bytes so far.
    frames: Deframer,
    /// Client to supervisor, sent and not yet read.
    unread: Vec<u8>,
    /// Bytes of client input the supervisor has read.
    read: usize,
    /// Bytes the client has sent.
    sent: usize,
    /// How the client's terminal answers queries, for property tests.
    terminal: Option<Terminal>,
    /// When the terminal has finished writing its last reply. It writes
    /// one reply at a time.
    terminal_free: Duration,
}

impl ClientIo for Client {
    type Sock<'a> = &'a mut Scripted;

    fn socket(&mut self) -> Option<&mut Scripted> {
        Some(&mut self.sock)
    }

    fn failed(&self) -> bool {
        false
    }
}

/// The payloads of the output frames the client receives, as they arrive.
#[derive(Default)]
struct Deframer {
    header: Vec<u8>,
    /// Payload bytes left in the current frame.
    left: usize,
}

impl Deframer {
    fn push(&mut self, mut bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            if self.left == 0 {
                let n = (HEADER - self.header.len()).min(bytes.len());
                self.header.extend_from_slice(&bytes[..n]);
                bytes = &bytes[n..];
                if self.header.len() == HEADER {
                    assert_eq!(self.header[4], crate::proto::kind::OUTPUT);
                    let len = u32::from_be_bytes(self.header[..4].try_into().unwrap());
                    self.left = len as usize - 1;
                    self.header.clear();
                }
                continue;
            }
            let n = self.left.min(bytes.len());
            out.extend_from_slice(&bytes[..n]);
            self.left -= n;
            bytes = &bytes[n..];
        }
        out
    }
}

/// A client's terminal: it answers each query it receives after `delay`,
/// writing the reply in two pieces `split` apart when `split` is set.
#[derive(Debug, Clone, Copy)]
struct Terminal {
    delay: Option<Duration>,
    split: Option<Duration>,
}

/// What happened besides the exchange's own accounting.
#[derive(Default)]
struct Record {
    detached: Vec<&'static str>,
    model_failures: usize,
    /// Per query, client replies that reached the program, by their end
    /// offset in that client's input.
    client_replies: Vec<(QueryId, usize)>,
}

struct Harness {
    now: Duration,
    exchange: Exchange<Client>,
    pty: Scripted,
    next_frame: u64,
    /// Program output read so far.
    fed: u64,
    /// The model's DSR 6 replies: when each arrives. `None` never replies.
    model_delay: Option<Duration>,
    model_replies: Vec<(Duration, QueryId, Vec<u8>)>,
    /// Client replies scheduled: when, for which client generation, query,
    /// bytes, and whether this piece ends a reply.
    terminal_writes: Vec<(Duration, u64, Option<QueryId>, Vec<u8>)>,
    client_gen: u64,
    /// Ends of client replies in flight, per client generation.
    reply_ends: Vec<(u64, QueryId, usize)>,
    kinds: BTreeMap<QueryId, Kind>,
    bracketed_paste: bool,
    rec: Record,
}

impl Harness {
    fn new() -> Harness {
        let mut pty = Scripted::new();
        pty.open();
        Harness {
            now: S,
            exchange: Exchange::with_input(Input::new()),
            pty,
            next_frame: 1,
            fed: 0,
            model_delay: Some(MS),
            model_replies: Vec::new(),
            terminal_writes: Vec::new(),
            client_gen: 0,
            reply_ends: Vec::new(),
            kinds: BTreeMap::new(),
            bracketed_paste: false,
            rec: Record::default(),
        }
    }

    fn client(&self) -> Option<&Client> {
        self.exchange.client().map(|c| &c.io)
    }

    fn client_mut(&mut self) -> Option<&mut Client> {
        self.exchange.client_mut().map(|c| &mut c.io)
    }

    /// What the program has read from its terminal since the last call.
    fn program_reads(&mut self) -> Vec<u8> {
        self.pty.take()
    }

    /// What the client has received since the last call, as output.
    fn client_receives(&mut self) -> Vec<u8> {
        let c = self.client_mut().expect("a client");
        let bytes = c.sock.take();
        c.frames.push(&bytes)
    }

    fn client_flow(&self) -> &ClientFlow {
        self.exchange.client().expect("a client").flow()
    }

    /// The supervisor's answers, in order.
    fn answers(&self) -> Vec<(QueryId, Vec<u8>)> {
        self.settled(|e| match e {
            queries::Event::Answer { id, bytes } => Some((*id, bytes.clone())),
            _ => None,
        })
    }

    fn unanswered(&self) -> Vec<(QueryId, NoReply)> {
        self.settled(|e| match e {
            queries::Event::Unanswered { id, why } => Some((*id, *why)),
            _ => None,
        })
    }

    fn expired(&self) -> Vec<QueryId> {
        self.settled(|e| match e {
            queries::Event::Expired { id } => Some(*id),
            _ => None,
        })
    }

    fn settled<T>(&self, f: impl Fn(&queries::Event) -> Option<T>) -> Vec<T> {
        self.exchange.settled.iter().filter_map(f).collect()
    }

    /// How many bytes the supervisor may read from the PTY.
    fn read_budget(&self) -> usize {
        self.exchange.read_budget()
    }

    /// The supervisor reads `bytes` of program output from the PTY. With a
    /// query, the bytes end with it, as the tracker cuts frames.
    fn output(&mut self, bytes: &[u8], query: Option<Kind>) -> Option<QueryId> {
        self.read(bytes, None, query)
    }

    /// [`Harness::output`], where a long sequence ends `long_end` bytes in.
    fn read(
        &mut self,
        bytes: &[u8],
        long_end: Option<usize>,
        query: Option<Kind>,
    ) -> Option<QueryId> {
        assert!(bytes.len() <= self.read_budget(), "read while paused");
        let id = QueryId(self.next_frame);
        self.next_frame += 1;
        let base = self.fed;
        self.fed += bytes.len() as u64;
        let mut cuts = Vec::new();
        if let Some(kind) = query {
            self.kinds.insert(id, kind);
            if kind == Kind::Dsr6
                && let Some(delay) = self.model_delay
            {
                let reply = format!("\x1b[{};1R", id.0).into_bytes();
                self.model_replies.push((self.now + delay, id, reply));
            }
            if self.exchange.query(id, kind, self.now) {
                cuts.push((self.fed, id));
            }
        }
        if let Some(end) = long_end {
            self.exchange.long_sequence_ended(base + end as u64);
        }
        self.exchange.client_output(bytes, base, &cuts, self.now);
        self.pump();
        query.map(|_| id)
    }

    fn send(&mut self, data: &[u8], paste: bool, submit: bool) -> Result<(), QueueFull> {
        let result = self
            .exchange
            .send(data, paste, submit, self.bracketed_paste);
        self.pump();
        result
    }

    /// The client's terminal writes `bytes` to the socket.
    fn client_writes(&mut self, bytes: &[u8]) {
        let client = self.client_mut().expect("a client");
        client.unread.extend_from_slice(bytes);
        client.sent += bytes.len();
    }

    /// The user types `bytes`. A terminal writes each reply in one piece, so
    /// a keystroke never lands inside one, even when the reply reaches the
    /// supervisor in two reads.
    fn client_types(&mut self, bytes: &[u8]) {
        let Some(client) = self.client() else { return };
        let at = self.now.max(client.terminal_free);
        let generation = self.client_gen;
        self.terminal_writes
            .push((at, generation, None, bytes.to_vec()));
        self.step();
    }

    /// The supervisor reads what the client has sent.
    fn read_client(&mut self) {
        let now = self.now;
        let Some(client) = self.client_mut() else {
            return;
        };
        let bytes = std::mem::take(&mut client.unread);
        client.read += bytes.len();
        let read = client.read;
        self.exchange.client_input(&bytes, now);
        let generation = self.client_gen;
        self.reply_ends.retain(|(g, id, end)| {
            if *g == generation && *end <= read {
                self.rec.client_replies.push((*id, *end));
                false
            } else {
                true
            }
        });
        self.pump();
    }

    /// The client reads up to `n` bytes from its socket.
    fn client_reads(&mut self, n: usize) {
        if let Some(client) = self.client_mut() {
            client.sock.allow(n);
        }
        self.pump();
    }

    /// A new client attaches, taking over from any other, and is sent
    /// `restore` once the snapshot is built.
    fn attach(&mut self, restore: &[u8]) {
        self.attach_with(restore, None);
    }

    fn attach_with(&mut self, restore: &[u8], terminal: Option<Terminal>) {
        self.connect(terminal);
        self.resync(restore);
    }

    /// A client attaches with no restore, inside a long sequence or not.
    fn attach_live(&mut self, in_long: bool, held: &[u8]) {
        self.connect(None);
        self.exchange.start_live(in_long, held, self.now);
        self.pump();
    }

    fn connect(&mut self, terminal: Option<Terminal>) {
        if self.exchange.client().is_some() {
            self.depart("takeover");
        }
        self.client_gen += 1;
        let client = Client {
            sock: Scripted::new(),
            frames: Deframer::default(),
            unread: Vec::new(),
            read: 0,
            sent: 0,
            terminal,
            terminal_free: self.now,
        };
        self.exchange.attach(client, self.now);
    }

    fn resync(&mut self, restore: &[u8]) {
        assert!(self.client_flow().snapshot_ready());
        let epoch = self.exchange.request_restore(false).expect("a client");
        // The model builds the snapshot at once here; nothing is read from
        // the PTY in between either way.
        assert_eq!(self.read_budget(), 0);
        assert!(self.exchange.restore_ready(epoch, restore, &[], self.now));
    }

    fn detach(&mut self) {
        self.depart("detach");
    }

    /// Before a client leaves, the supervisor reads what it already sent,
    /// then answers what is still outstanding, then discards its input.
    fn depart(&mut self, why: &'static str) {
        self.read_client();
        let (_, needs) = self
            .exchange
            .client_leaves(Vec::new(), self.now)
            .expect("a client");
        let generation = self.client_gen;
        self.reply_ends.retain(|(g, _, _)| *g != generation);
        self.terminal_writes.retain(|(_, g, _, _)| *g != generation);
        self.rec.detached.push(why);
        self.act(needs);
        self.pump();
    }

    fn set_model(&mut self, state: ModelState) {
        if state != ModelState::Ok {
            // A new generation: replies from the old one are discarded.
            self.model_replies.clear();
        }
        self.exchange.model_state(state);
        self.pump();
    }

    /// Time passes by `dt`; everything due happens.
    fn advance(&mut self, dt: Duration) {
        let end = self.now + dt;
        loop {
            let next = [
                self.exchange.next_deadline(),
                self.model_replies.iter().map(|r| r.0).min(),
                self.terminal_writes.iter().map(|w| w.0).min(),
            ]
            .into_iter()
            .flatten()
            .filter(|t| *t <= end)
            .min();
            let Some(t) = next else { break };
            self.now = self.now.max(t);
            self.step();
        }
        self.now = end;
        self.step();
    }

    fn step(&mut self) {
        let now = self.now;
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.model_replies)
            .into_iter()
            .partition(|r| r.0 <= now);
        self.model_replies = rest;
        for (_, id, bytes) in due {
            self.exchange.model_reply(id, &bytes, now);
        }
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.terminal_writes)
            .into_iter()
            .partition(|w| w.0 <= now);
        self.terminal_writes = rest;
        for (_, generation, end_of, bytes) in due {
            if generation == self.client_gen && self.exchange.client().is_some() {
                self.client_writes(&bytes);
                if let Some(id) = end_of {
                    let sent = self.client().map_or(0, |c| c.sent);
                    self.reply_ends.push((generation, id, sent));
                }
            }
        }
        let needs = self.exchange.tick(now);
        self.act(needs);
        self.pump();
    }

    /// Writes what the client's socket and the PTY accept, as the
    /// supervisor's loop does, again after each need it acts on.
    fn pump(&mut self) {
        loop {
            let writable = self.client().is_some_and(|c| c.sock.writable());
            let needs = self.exchange.flush_client(writable, self.now);
            let delivered = std::mem::take(&mut self.exchange.delivered);
            self.terminal_receives(&delivered);
            if needs.is_empty() {
                break;
            }
            self.act(needs);
        }
        self.exchange.flush_input(&mut self.pty);
    }

    fn act(&mut self, needs: Vec<Need>) {
        for need in needs {
            match need {
                Need::FailModel => {
                    self.rec.model_failures += 1;
                    self.set_model(ModelState::Failed);
                }
                Need::Resync => self.resync(b"<resync>"),
                Need::DetachTooSlow => self.depart("too_slow"),
                Need::ClientGone => unreachable!("scripted sockets do not fail"),
            }
        }
    }

    fn terminal_receives(&mut self, ids: &[QueryId]) {
        let Some(Terminal {
            delay: Some(delay),
            split,
        }) = self.client().and_then(|c| c.terminal)
        else {
            return;
        };
        for id in ids {
            let reply = match self.kinds[id] {
                Kind::Dsr5 => b"\x1b[0n".to_vec(),
                Kind::Dsr6 => format!("\x1b[{};2R", id.0).into_bytes(),
                Kind::Da1 => format!("\x1b[?62;{}c", id.0).into_bytes(),
                Kind::Da2 => format!("\x1b[>{};0c", id.0).into_bytes(),
            };
            let now = self.now;
            let client = self.client_mut().expect("a client");
            let at = (now + delay).max(client.terminal_free);
            client.terminal_free = at + split.unwrap_or_default();
            let generation = self.client_gen;
            match split {
                Some(gap) => {
                    let (a, b) = reply.split_at(3);
                    self.terminal_writes
                        .push((at, generation, None, a.to_vec()));
                    self.terminal_writes
                        .push((at + gap, generation, Some(*id), b.to_vec()));
                }
                None => self
                    .terminal_writes
                    .push((at, generation, Some(*id), reply)),
            }
        }
    }
}

fn ids(list: &[(QueryId, Vec<u8>)]) -> Vec<u64> {
    list.iter().map(|(id, _)| id.0).collect()
}

#[test]
fn detached_queries_are_answered_with_fixed_and_model_replies() {
    let mut h = Harness::new();
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    h.output(b"\x1b[c", Some(Kind::Da1));
    h.output(b"\x1b[>c", Some(Kind::Da2));
    h.output(b"prompt> \x1b[6n", Some(Kind::Dsr6));
    assert_eq!(h.program_reads(), b"\x1b[0n\x1b[?1;2c\x1b[>0;0;0c");
    h.advance(MS);
    assert_eq!(h.program_reads(), b"\x1b[4;1R");
    assert!(h.exchange.queries_idle());
}

#[test]
fn a_reply_behind_a_large_send_goes_first_unless_the_send_has_started() {
    let mut h = Harness::new();
    h.attach(b"<restore>");
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"<restore>");
    // The PTY takes nothing while the program is busy.
    h.pty = Scripted::new();
    let big = vec![b'x'; 900 * 1024];
    h.send(&big, false, true).unwrap();
    let id = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    assert_eq!(h.client_receives(), b"\x1b[6n");
    h.client_writes(format!("\x1b[{};2R", id.0).as_bytes());
    h.read_client();
    h.send(b"after", false, false).unwrap();
    h.pty.open();
    h.advance(MS);
    let out = h.program_reads();
    assert_eq!(&out[..6], b"\x1b[1;2R");
    assert_eq!(&out[6..6 + big.len()], &big[..]);
    assert_eq!(&out[6 + big.len()..], b"\rafter");
    // Once part of the send is written, the reply waits for its end.
    h.pty = Scripted::new();
    h.send(b"0123456789", false, false).unwrap();
    h.pty.allow(4);
    h.advance(MS);
    let id = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.client_reads(usize::MAX);
    h.client_writes(format!("\x1b[{};2R", id.0).as_bytes());
    h.read_client();
    h.pty.open();
    h.advance(MS);
    assert_eq!(h.program_reads(), b"0123456789\x1b[2;2R");
    assert!(h.answers().is_empty(), "the client answered both");
}

#[test]
fn a_client_reply_split_across_reads_still_goes_first() {
    let mut h = Harness::new();
    h.attach(b"<restore>");
    h.client_reads(usize::MAX);
    h.output(b"\x1b[6n", Some(Kind::Dsr6));
    h.pty = Scripted::new();
    h.send(b"queued send", false, false).unwrap();
    h.client_writes(b"ab\x1b[1");
    h.read_client();
    h.advance(60 * MS);
    h.client_writes(b";2Rcd");
    h.read_client();
    h.pty.open();
    h.advance(MS);
    assert_eq!(h.program_reads(), b"\x1b[1;2Rqueued sendabcd");
    h.detach();
    assert!(h.answers().is_empty());
}

#[test]
fn a_stall_drops_unstarted_frames_and_the_supervisor_answers_their_queries() {
    let mut h = Harness::new();
    h.attach(b"<restore>");
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"<restore>");
    // The client takes part of the first frame, then stops reading.
    h.client_mut().unwrap().sock = Scripted::new();
    h.output(b"one \x1b[c", Some(Kind::Da1));
    h.output(b"two", None);
    h.output(b"three", None);
    h.client_reads(HEADER + 2);
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    h.output(b"\x1b[6n", Some(Kind::Dsr6));
    assert_eq!(h.client_receives(), b"on");
    h.advance(4999 * MS);
    assert_eq!(h.program_reads(), b"");
    h.advance(MS);
    // 5 seconds without progress: the DSR 5 and DSR 6 frames were never
    // started, so the supervisor answers them, in order. The DA1 frame was
    // started, stays, and is the client's to answer.
    assert_eq!(h.program_reads(), b"\x1b[0n\x1b[5;1R");
    assert_eq!(h.client_flow().phase(), Phase::Lagging);
    assert_eq!(h.client_flow().read_budget(), usize::MAX);
    // While it lags, output and its queries are the supervisor's.
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    assert_eq!(h.program_reads(), b"\x1b[0n");
    // The client wakes, finishes the started frame and gets a resync.
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"e \x1b[c<resync>");
    assert_eq!(h.client_flow().phase(), Phase::Live);
    h.client_writes(b"\x1b[?62c");
    h.read_client();
    assert_eq!(h.program_reads(), b"\x1b[?62c");
    h.detach();
    assert_eq!(ids(&h.answers()), [4, 5, 6]);
}

#[test]
fn a_stall_mid_restore_is_followed_by_a_resync() {
    let mut h = Harness::new();
    h.output(b"before", None);
    h.attach(b"<a long restore>");
    h.client_reads(4);
    assert_eq!(h.read_budget(), 0);
    h.advance(5 * S);
    assert_eq!(h.client_flow().phase(), Phase::Lagging);
    h.output(b"while lagging \x1b[6n", Some(Kind::Dsr6));
    h.advance(MS);
    assert_eq!(h.program_reads(), b"\x1b[2;1R");
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"<a long restore><resync>");
}

#[test]
fn three_stalls_in_ten_minutes_detach_the_client_as_too_slow() {
    let mut h = Harness::new();
    h.attach(b"R");
    h.client_reads(usize::MAX);
    for round in 0..3 {
        h.client_mut().unwrap().sock = Scripted::new();
        h.output(b"x", None);
        h.output(b"\x1b[c", Some(Kind::Da1));
        h.advance(5 * S);
        if round < 2 {
            assert_eq!(h.client_flow().phase(), Phase::Lagging);
            h.client_reads(usize::MAX);
            assert_eq!(h.client_flow().phase(), Phase::Live);
        }
    }
    assert!(h.exchange.client().is_none());
    assert_eq!(h.rec.detached, ["too_slow"]);
    assert_eq!(h.program_reads(), b"\x1b[?1;2c\x1b[?1;2c\x1b[?1;2c");
}

#[test]
fn takeover_reads_the_old_clients_input_before_answering() {
    let mut h = Harness::new();
    h.attach(b"<restore A>");
    h.client_reads(usize::MAX);
    let da1 = h.output(b"\x1b[c", Some(Kind::Da1)).unwrap();
    let dsr6 = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.advance(MS);
    assert_eq!(h.client_receives(), b"<restore A>\x1b[c\x1b[6n");
    // The old terminal has answered DA1, but the supervisor has not read it.
    h.client_writes(b"typed\x1b[?62;22c");
    h.attach(b"<restore B>");
    // The old terminal's reply goes ahead of its typed bytes, and the
    // supervisor answers only the query it had not answered.
    assert_eq!(h.program_reads(), b"\x1b[?62;22ctyped\x1b[2;1R");
    assert_eq!(h.answers(), [(dsr6, b"\x1b[2;1R".to_vec())]);
    assert!(!h.answers().iter().any(|(id, _)| *id == da1));
    assert_eq!(h.rec.detached, ["takeover"]);
    // The new client owns the queries read from now on.
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"<restore B>");
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    assert_eq!(h.client_receives(), b"\x1b[5n");
    assert_eq!(h.program_reads(), b"");
}

#[test]
fn takeover_during_a_restore() {
    let mut h = Harness::new();
    h.attach(b"<restore A>");
    h.client_reads(3);
    h.attach(b"<restore B>");
    h.client_reads(usize::MAX);
    assert_eq!(h.client_receives(), b"<restore B>");
    assert_eq!(h.read_budget(), flow::LIVE_CAP);
}

#[test]
fn a_detach_answers_every_outstanding_query_once() {
    let mut h = Harness::new();
    h.attach(b"R");
    h.client_reads(usize::MAX);
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    h.output(b"\x1b[6n", Some(Kind::Dsr6));
    h.output(b"\x1b[>c", Some(Kind::Da2));
    h.advance(MS);
    h.detach();
    assert_eq!(h.program_reads(), b"\x1b[0n\x1b[2;1R\x1b[>0;0;0c");
    h.advance(10 * S);
    assert_eq!(h.program_reads(), b"");
    assert!(h.exchange.queries_idle());
}

#[test]
fn an_unanswered_forwarded_query_expires_after_two_seconds() {
    let mut h = Harness::new();
    h.attach(b"R");
    h.client_reads(usize::MAX);
    let id = h.output(b"\x1b[>c", Some(Kind::Da2)).unwrap();
    h.advance(1999 * MS);
    assert!(h.expired().is_empty());
    h.advance(MS);
    assert_eq!(h.expired(), [id]);
    // A detach now answers nothing; the program waits for its own timeout,
    // as with a terminal that does not support the query.
    h.detach();
    assert_eq!(h.program_reads(), b"");
    assert!(h.answers().is_empty());
}

#[test]
fn a_keystroke_shaped_like_a_reply_can_consume_a_query() {
    // The documented limit: Shift+F3 is counted as the DSR 6 reply, and a
    // detach before the real reply leaves the query unanswered.
    let mut h = Harness::new();
    h.attach(b"R");
    h.client_reads(usize::MAX);
    h.output(b"\x1b[6n", Some(Kind::Dsr6));
    h.client_writes(b"\x1b[1;2R");
    h.read_client();
    h.detach();
    assert_eq!(h.program_reads(), b"\x1b[1;2R");
    assert!(h.answers().is_empty());
}

#[test]
fn every_unanswered_dsr6_case() {
    let mut h = Harness::new();
    // The model misses the deadline.
    h.model_delay = None;
    let missed = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.advance(500 * MS);
    // The model replies, but after the deadline.
    h.model_delay = Some(600 * MS);
    let late = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.advance(S);
    // The third miss within a minute fails the model.
    let third = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.advance(S);
    assert_eq!(h.rec.model_failures, 1);
    // A failed model, then a partial one, answer nothing and count nothing.
    let failed = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.set_model(ModelState::Partial);
    h.model_delay = Some(MS);
    let partial = h.output(b"\x1b[6n", Some(Kind::Dsr6)).unwrap();
    h.advance(S);
    // DSR 5 is still answered.
    h.output(b"\x1b[5n", Some(Kind::Dsr5));
    // After RIS the model is complete again.
    h.set_model(ModelState::Ok);
    h.output(b"\x1b[6n", Some(Kind::Dsr6));
    h.advance(MS);
    assert_eq!(h.program_reads(), b"\x1b[0n\x1b[7;1R");
    assert_eq!(
        h.unanswered(),
        [
            (missed, NoReply::ModelMissedDeadline),
            (late, NoReply::ModelMissedDeadline),
            (third, NoReply::ModelMissedDeadline),
            (failed, NoReply::ModelUnavailable),
            (partial, NoReply::ModelUnavailable),
        ]
    );
    assert_eq!(h.rec.model_failures, 1);
}

#[test]
fn a_query_held_back_at_the_snapshot_completes_in_the_clients_output() {
    // Ownership passes at the snapshot request; output read before it is
    // the supervisor's, output after it the client's.
    let mut h = Harness::new();
    h.output(b"\x1b[c", Some(Kind::Da1));
    h.attach(b"R");
    h.client_reads(usize::MAX);
    h.output(b"\x1b[c", Some(Kind::Da1));
    assert_eq!(h.program_reads(), b"\x1b[?1;2c");
    assert_eq!(h.client_receives(), b"R\x1b[c");
}

#[test]
fn a_client_attached_inside_a_long_sequence_gets_output_from_its_end() {
    let mut h = Harness::new();
    h.attach_live(true, b"\x1b_Gstart");
    h.client_reads(usize::MAX);
    // The rest of the sequence, withheld; then a read that ends it, with a
    // query after the end.
    h.output(b"more of it", None);
    assert_eq!(h.client_receives(), b"");
    let id = h
        .read(b"end\x1b\\after\x1b[6n", Some(5), Some(Kind::Dsr6))
        .unwrap();
    assert_eq!(h.client_receives(), b"after\x1b[6n");
    // The query is the client's, and its frame reached it.
    h.client_writes(format!("\x1b[{};2R", id.0).as_bytes());
    h.read_client();
    h.advance(10 * S);
    assert_eq!(h.program_reads(), format!("\x1b[{};2R", id.0).as_bytes());
    assert!(h.answers().is_empty());
    assert!(h.exchange.queries_idle());
}

#[test]
fn a_client_attached_between_sequences_gets_the_held_bytes_first() {
    let mut h = Harness::new();
    h.attach_live(false, b"\x1b[3");
    h.client_reads(usize::MAX);
    h.output(b"1mred", None);
    assert_eq!(h.client_receives(), b"\x1b[31mred");
}

#[test]
fn paste_follows_the_programs_bracketed_paste_mode() {
    let mut h = Harness::new();
    h.send(b"a", true, true).unwrap();
    h.bracketed_paste = true;
    h.send(b"b", true, true).unwrap();
    h.send(b"c", false, true).unwrap();
    assert_eq!(h.program_reads(), b"a\r\x1b[200~b\x1b[201~\rc\r");
}

// Property tests.

#[derive(Debug, Clone)]
enum Op {
    Output(usize, Option<Kind>),
    Attach(Option<u16>, Option<u16>),
    Detach,
    ClientReads(usize),
    ClientStops,
    ReadClient,
    Type(Vec<u8>),
    Send(usize),
    Advance(u16),
    ModelFails,
    ModelRestarts,
    ModelComplete,
    ModelDelay(Option<u16>),
    PtyBlocked(bool),
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        Just(Kind::Dsr5),
        Just(Kind::Dsr6),
        Just(Kind::Da1),
        Just(Kind::Da2)
    ]
}

fn keystrokes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"x".to_vec()),
        Just(b"\x1b".to_vec()),
        Just(b"\x1b[".to_vec()),
        Just(b"\x1b[A".to_vec()),
        // Shapes of replies, typed.
        Just(b"\x1b[1;2R".to_vec()),
        Just(b"\x1b[0n".to_vec()),
        Just(b"\x1b[?1c".to_vec()),
        Just(b"\x1b[>1c".to_vec()),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (1usize..64, proptest::option::weighted(0.6, kind()))
            .prop_map(|(n, k)| Op::Output(n, k)),
        1 => (
            proptest::option::weighted(0.8, 0u16..2500),
            proptest::option::of(0u16..200)
        )
            .prop_map(|(d, s)| Op::Attach(d, s)),
        1 => Just(Op::Detach),
        4 => prop_oneof![1usize..32, Just(usize::MAX)].prop_map(Op::ClientReads),
        2 => Just(Op::ClientStops),
        3 => Just(Op::ReadClient),
        2 => keystrokes().prop_map(Op::Type),
        1 => (0usize..2000).prop_map(Op::Send),
        5 => prop_oneof![0u16..50, 50u16..700, 1900u16..2100, 4900u16..5200]
            .prop_map(Op::Advance),
        1 => prop_oneof![
            Just(Op::ModelFails),
            Just(Op::ModelRestarts),
            Just(Op::ModelComplete)
        ],
        1 => proptest::option::weighted(0.8, 0u16..800).prop_map(Op::ModelDelay),
        1 => any::<bool>().prop_map(Op::PtyBlocked),
    ]
}

fn run(ops: &[Op]) -> Harness {
    let mut h = Harness::new();
    for op in ops {
        match op {
            Op::Output(n, kind) => {
                if h.read_budget() >= *n {
                    let mut bytes = vec![b'.'; *n];
                    if kind.is_some() {
                        bytes.extend_from_slice(b"\x1b[6n");
                    }
                    h.output(&bytes, *kind);
                }
            }
            Op::Attach(delay, split) => {
                let terminal = Terminal {
                    delay: delay.map(|d| MS * u32::from(d)),
                    split: split.map(|s| MS * u32::from(s)),
                };
                h.attach_with(b"<restore>", Some(terminal));
            }
            Op::Detach => {
                if h.exchange.client().is_some() {
                    h.detach();
                }
            }
            Op::ClientReads(n) => h.client_reads(*n),
            Op::ClientStops => {
                if let Some(client) = h.client_mut() {
                    client.sock.close();
                }
            }
            Op::ReadClient => h.read_client(),
            Op::Type(bytes) => h.client_types(bytes),
            Op::Send(n) => {
                let _ = h.send(&vec![b's'; *n], false, true);
            }
            Op::Advance(ms) => h.advance(MS * u32::from(*ms)),
            Op::ModelFails => h.set_model(ModelState::Failed),
            Op::ModelRestarts => h.set_model(ModelState::Partial),
            Op::ModelComplete => h.set_model(ModelState::Ok),
            Op::ModelDelay(d) => h.model_delay = d.map(|d| MS * u32::from(d)),
            Op::PtyBlocked(blocked) => {
                if *blocked {
                    let out = h.pty.take();
                    h.pty = Scripted::new();
                    h.pty.out = out;
                } else {
                    h.pty.open();
                    h.pump();
                }
            }
        }
    }
    h
}

/// Replies per query: the supervisor's answers plus the client replies that
/// reached the program.
fn replies(h: &Harness) -> BTreeMap<QueryId, usize> {
    let mut count = BTreeMap::new();
    for (id, _) in &h.answers() {
        *count.entry(*id).or_default() += 1;
    }
    for (id, _) in &h.rec.client_replies {
        *count.entry(*id).or_default() += 1;
    }
    count
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

    #[test]
    fn no_scenario_gives_two_replies_to_one_query(ops in proptest::collection::vec(op(), 0..80)) {
        let mut h = run(&ops);
        // Everything still in flight settles: the client leaves, and time
        // passes past every deadline.
        if h.exchange.client().is_some() {
            h.detach();
        }
        h.pty.open();
        h.advance(10 * S);
        h.advance(10 * S);
        for (id, n) in replies(&h) {
            prop_assert!(n <= 1, "query {id:?} got {n} replies");
        }
        prop_assert!(h.exchange.queries_idle(), "every query settles");
        prop_assert_eq!(h.exchange.input().queued(), 0, "every reply is written");
        let settled: Vec<QueryId> = h.answers().iter().map(|a| a.0)
            .chain(h.unanswered().iter().map(|u| u.0))
            .chain(h.expired().iter().copied())
            .collect();
        let mut unique = settled.clone();
        unique.sort();
        unique.dedup();
        prop_assert_eq!(unique.len(), settled.len(), "settled once each");
    }
}

#[derive(Debug, Clone)]
enum InputOp {
    Send(usize, bool),
    Reply,
    Type(Vec<u8>),
    Flush(usize),
}

fn input_op() -> impl Strategy<Value = InputOp> {
    prop_oneof![
        3 => (0usize..400, any::<bool>()).prop_map(|(n, s)| InputOp::Send(n, s)),
        2 => Just(InputOp::Reply),
        2 => proptest::collection::vec(b'a'..=b'z', 1..20).prop_map(InputOp::Type),
        4 => (0usize..300).prop_map(InputOp::Flush),
    ]
}

proptest! {
    #[test]
    fn replies_never_split_an_item_and_items_keep_fifo_order(
        ops in proptest::collection::vec(input_op(), 0..60)
    ) {
        struct NoQueries;
        impl queries::ReplyMatcher for NoQueries {
            fn outstanding(&self) -> bool { false }
            fn match_reply(&mut self, _: Kind) -> bool { false }
        }
        let mut input = Input::new();
        let mut pty = Scripted::new();
        let mut items: Vec<Vec<u8>> = Vec::new();
        let mut replies: Vec<Vec<u8>> = Vec::new();
        for op in &ops {
            match op {
                InputOp::Send(n, submit) => {
                    let data = format!("<S{}:{}>", items.len(), "s".repeat(*n));
                    input.send(data.as_bytes(), false, *submit, false).unwrap();
                    let mut item = data.into_bytes();
                    if *submit {
                        item.push(b'\r');
                    }
                    items.push(item);
                }
                InputOp::Reply => {
                    let reply = format!("\x1b[{};9R", replies.len()).into_bytes();
                    input.push_reply(reply.clone());
                    replies.push(reply);
                }
                InputOp::Type(bytes) => {
                    input.client_read(bytes, Duration::ZERO, &mut NoQueries);
                    items.push(bytes.clone());
                }
                InputOp::Flush(n) => {
                    pty.allow(*n);
                    input.flush(&mut pty);
                }
            }
        }
        pty.open();
        input.flush(&mut pty);
        let out = pty.take();
        // The output parses as whole replies and whole items, each kind in
        // its own order. Items never start with ESC and replies always do,
        // so the parse is unambiguous.
        let (mut p, mut r, mut i) = (0, 0, 0);
        while p < out.len() {
            if r < replies.len() && out[p..].starts_with(&replies[r]) {
                p += replies[r].len();
                r += 1;
            } else if i < items.len() && out[p..].starts_with(&items[i]) {
                p += items[i].len();
                i += 1;
            } else {
                prop_assert!(false, "split or reordered at {}: {:?}", p, out);
            }
        }
        prop_assert_eq!((r, i), (replies.len(), items.len()));
    }
}
