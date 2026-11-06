// SPDX-License-Identifier: GPL-3.0-only

//! The exchange: a session's terminal traffic between the program, the
//! attached client and Perch's own answers, with no descriptors and no
//! clock of its own.
//!
//! It owns the three parts that must move together: who answers each of
//! the program's queries ([`Queries`]), what reaches the program's input
//! and in what order ([`Input`]), and what the attached client is sent and
//! how fast ([`ClientFlow`]). The supervisor feeds it what it reads and
//! acts on each [`Need`] it returns; `scenarios.rs` drives the same exchange
//! with scripted sockets.
//!
//! The invariant the parts keep together: the client owns the queries read
//! from its snapshot request on exactly while it gets live output, and each
//! query it owns ends a frame of its own, so a stall can tell which queries
//! never reached it.

use std::time::Duration;

use crate::flow::{self, ClientFlow, Frame};
use crate::input::{Input, QueueFull};
use crate::proto::{self, kind};
use crate::queries::{self, Kind, ModelState, Queries, QueryId};
use crate::supervisor::log::log;
use crate::transport::Transport;

/// The most raw bytes in one output frame.
const MAX_PAYLOAD: usize = proto::MAX_FRAME - 1;

/// The attached client's socket, as the exchange writes output to it.
pub trait ClientIo {
    type Sock<'a>: Transport
    where
        Self: 'a;
    /// The socket for output frames once everything queued ahead of them
    /// (control frames) is written, or `None` while some still waits.
    fn socket(&mut self) -> Option<Self::Sock<'_>>;
    /// Writing to the socket failed for good: the client is gone.
    fn failed(&self) -> bool;
}

/// The attached client: its flow, and the caller's own state (`io`).
pub struct Attached<C> {
    flow: ClientFlow,
    /// Output is withheld: the client attached in the middle of a long
    /// sequence, which has not ended yet.
    withhold: bool,
    /// Where the withheld long sequence ended, in the read now being
    /// passed on: the client's output starts there.
    resume_at: Option<u64>,
    pub io: C,
}

impl<C> Attached<C> {
    pub fn flow(&self) -> &ClientFlow {
        &self.flow
    }
}

/// What the exchange cannot do itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The model missed too many DSR 6 deadlines: fail it.
    FailModel,
    /// The client lagged and has caught up: request a `screen` restore.
    Resync,
    /// The client stalled 3 times within 10 minutes: detach it as too
    /// slow.
    DetachTooSlow,
    /// Writing to the client failed: it is gone.
    ClientGone,
}

/// See the module documentation.
pub struct Exchange<C> {
    queries: Queries,
    input: Input,
    client: Option<Attached<C>>,
    /// Every query event, for the tests' accounting.
    #[cfg(test)]
    pub(crate) settled: Vec<queries::Event>,
    /// Queries whose frames reached the client, for the tests' terminal.
    #[cfg(test)]
    pub(crate) delivered: Vec<QueryId>,
}

impl<C: ClientIo> Default for Exchange<C> {
    fn default() -> Self {
        Exchange::new()
    }
}

impl<C: ClientIo> Exchange<C> {
    pub fn new() -> Exchange<C> {
        Exchange::with_input(Input::new())
    }

    /// An exchange whose program input is `input`, as configured.
    pub fn with_input(input: Input) -> Exchange<C> {
        Exchange {
            queries: Queries::new(),
            input,
            client: None,
            #[cfg(test)]
            settled: Vec::new(),
            #[cfg(test)]
            delivered: Vec::new(),
        }
    }

    pub fn client(&self) -> Option<&Attached<C>> {
        self.client.as_ref()
    }

    pub fn client_mut(&mut self) -> Option<&mut Attached<C>> {
        self.client.as_mut()
    }

    /// The program's input queue, for its state.
    pub fn input(&self) -> &Input {
        &self.input
    }

    /// Whether every query is settled.
    pub fn queries_idle(&self) -> bool {
        self.queries.is_idle()
    }

    /// How many bytes of program output may be read now.
    pub fn read_budget(&self) -> usize {
        self.client
            .as_ref()
            .map_or(usize::MAX, |c| c.flow.read_budget())
    }

    // -----------------------------------------------------------------
    // The program's output

    /// Query `id` was read in the program's output now. Returns whether it
    /// is the client's: its output frame must end with it.
    pub fn query(&mut self, id: QueryId, kind: Kind, now: Duration) -> bool {
        let client_owns = self.queries.client_owns();
        debug_assert_eq!(
            client_owns,
            self.client
                .as_ref()
                .is_some_and(|c| c.flow.phase() != flow::Phase::Lagging),
            "the client owns queries exactly while it gets live output"
        );
        let events = self.queries.query(id, kind, now);
        let needs = self.apply(events);
        debug_assert!(needs.is_empty(), "a new query misses no deadline");
        client_owns
    }

    /// Output read at offset `base`, for the client: cut after each query
    /// it owns (`cuts`, ends by offset), and nothing before the end of a
    /// long sequence the client was withholding.
    pub fn client_output(
        &mut self,
        chunk: &[u8],
        base: u64,
        cuts: &[(u64, QueryId)],
        now: Duration,
    ) {
        let Some(c) = self.client.as_mut() else {
            return;
        };
        if c.withhold {
            return;
        }
        let mut pos = c.resume_at.take().map_or(0, |at| {
            usize::try_from(at - base)
                .expect("within the chunk")
                .min(chunk.len())
        });
        for &(end, id) in cuts {
            let end = usize::try_from(end - base).expect("within the chunk");
            if end <= pos {
                continue;
            }
            let frame = Frame::with_query(output_frame(&chunk[pos..end]), id);
            if !c.flow.push_live(frame, now) {
                return;
            }
            pos = end;
        }
        if pos < chunk.len() {
            c.flow
                .push_live(Frame::new(output_frame(&chunk[pos..])), now);
        }
    }

    /// The long sequence ended at offset `at` of the read now being
    /// passed on. A client withholding it gets the output after it.
    pub fn long_sequence_ended(&mut self, at: u64) {
        if let Some(c) = self.client.as_mut()
            && c.withhold
        {
            c.withhold = false;
            c.resume_at = Some(at);
        }
    }

    // -----------------------------------------------------------------
    // The model

    /// The model's state changed.
    pub fn model_state(&mut self, state: ModelState) {
        let events = self.queries.set_model(state);
        let needs = self.apply(events);
        debug_assert!(needs.is_empty(), "a model state misses no deadline");
    }

    /// The model's DSR 6 reply to query `id`.
    pub fn model_reply(&mut self, id: QueryId, bytes: &[u8], now: Duration) {
        let events = self.queries.model_reply(id, bytes, now);
        let needs = self.apply(events);
        debug_assert!(needs.is_empty(), "a model reply misses no deadline");
    }

    // -----------------------------------------------------------------
    // The program's input

    /// An explicit send, with the program's bracketed paste mode.
    pub fn send(
        &mut self,
        data: &[u8],
        paste: bool,
        submit: bool,
        paste_on: bool,
    ) -> Result<(), QueueFull> {
        self.input.send(data, paste, submit, paste_on)
    }

    /// Writes queued input and replies to the program, as far as `pty`
    /// takes them.
    pub fn flush_input(&mut self, pty: &mut impl Transport) {
        self.input.flush(pty);
    }

    // -----------------------------------------------------------------
    // The client

    /// A client attaches. The caller has made any previous one leave.
    pub fn attach(&mut self, io: C, now: Duration) {
        debug_assert!(self.client.is_none(), "a client is attached already");
        self.client = Some(Attached {
            flow: ClientFlow::new(now),
            withhold: false,
            resume_at: None,
            io,
        });
    }

    /// The client gets live output from now on, with no restore: the
    /// bytes the feeder holds back (`held`) first, unless it attached in
    /// the middle of a long sequence (`in_long`).
    pub fn start_live(&mut self, in_long: bool, held: &[u8], now: Duration) {
        let Some(c) = self.client.as_mut() else {
            return;
        };
        self.queries.snapshot_requested();
        c.withhold = in_long;
        if !in_long {
            push_held(&mut c.flow, held, now);
        }
    }

    /// The program starts at the creator's attach: its output is the
    /// client's from the first byte.
    pub fn start_with_program(&mut self) {
        if self.client.is_some() {
            self.queries.snapshot_requested();
        }
    }

    /// A restore is requested for the client: program output pauses until
    /// it is enqueued, and queries read from now on are the client's.
    /// Returns the request's epoch.
    pub fn request_restore(&mut self, in_long: bool) -> Option<u64> {
        let c = self.client.as_mut()?;
        let epoch = c.flow.request_snapshot();
        self.queries.snapshot_requested();
        c.withhold = in_long;
        Some(epoch)
    }

    /// The restore for request `epoch` is built: it goes ahead of live
    /// output, then the bytes the feeder held back at the request. Returns
    /// whether it was taken; a later request supersedes it.
    pub fn restore_ready(&mut self, epoch: u64, bytes: &[u8], held: &[u8], now: Duration) -> bool {
        let Some(c) = self.client.as_mut() else {
            return false;
        };
        let frames = bytes
            .chunks(MAX_PAYLOAD)
            .map(|b| Frame::new(output_frame(b)))
            .collect();
        if !c.flow.restore_enqueued(epoch, frames, now) {
            return false;
        }
        if !c.withhold {
            push_held(&mut c.flow, held, now);
        }
        true
    }

    /// Room in the input queue for the client's next input.
    pub fn input_room(&self) -> usize {
        self.input.room()
    }

    /// Input the client typed, or its terminal's replies. The caller keeps
    /// what does not fit in [`Exchange::input_room`] until it does.
    pub fn client_input(&mut self, bytes: &[u8], now: Duration) {
        self.input.client_read(bytes, now, &mut self.queries);
    }

    /// The client stops being attached. `input` is what it sent and the
    /// caller has not passed on yet; it goes to the program first, as far
    /// as the queue has room. Then the supervisor answers what is still
    /// outstanding, and later input from it is discarded.
    pub fn client_leaves(
        &mut self,
        input: Vec<Vec<u8>>,
        now: Duration,
    ) -> Option<(Attached<C>, Vec<Need>)> {
        let c = self.client.take()?;
        for bytes in input {
            let room = self.input.room();
            if bytes.len() > room {
                note(&format!(
                    "the input queue is full: {} bytes of a leaving client's input were dropped",
                    bytes.len() - room
                ));
            }
            self.client_input(&bytes[..bytes.len().min(room)], now);
        }
        self.input.client_left();
        let events = self.queries.client_left(now);
        let needs = self.apply(events);
        Some((c, needs))
    }

    /// The session ended with the client attached: it goes, with nothing
    /// more to answer or pass on.
    pub fn client_ends(&mut self) -> Option<Attached<C>> {
        self.client.take()
    }

    /// Writes what the client is due, as far as its socket takes it
    /// (`writable`: the socket was ready), and applies the stall rule.
    pub fn flush_client(&mut self, writable: bool, now: Duration) -> Vec<Need> {
        let Some(c) = self.client.as_mut() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let ready = match c.io.socket() {
            Some(mut sock) => {
                events.extend(c.flow.flush(&mut sock, now));
                true
            }
            None => false,
        };
        if writable && ready {
            events.extend(c.flow.writable());
        }
        events.extend(c.flow.tick(now));
        if c.io.failed() {
            return vec![Need::ClientGone];
        }
        let mut needs = Vec::new();
        for e in events {
            match e {
                flow::Event::Delivered(ids) => {
                    self.queries.delivered(&ids, now);
                    #[cfg(test)]
                    self.delivered.extend(ids);
                }
                flow::Event::RestoreDelivered => {}
                flow::Event::Stalled { dropped } => {
                    note("the client stalled; output was dropped and it will be resynced");
                    let events = self.queries.stalled(&dropped, now);
                    needs.extend(self.apply(events));
                }
                flow::Event::TooSlow => {
                    needs.push(Need::DetachTooSlow);
                    return needs;
                }
                // Always the batch's last: it needs an empty queue, where
                // the stall rule finds nothing.
                flow::Event::ResyncDue => needs.push(Need::Resync),
            }
        }
        needs
    }

    /// Whether the client's socket should be polled for writing, given
    /// whether control frames wait (`queued`).
    pub fn client_wants_write(&self, queued: bool) -> bool {
        self.client.as_ref().is_some_and(|c| {
            queued || c.flow.pending().is_some() || c.flow.phase() == flow::Phase::Lagging
        })
    }

    /// Whether the client has been sent everything it is due, or never
    /// will be, apart from a restore the caller still waits on.
    pub fn client_drained(&self) -> bool {
        self.client
            .as_ref()
            .is_none_or(|c| c.flow.pending().is_none() || c.flow.phase() == flow::Phase::Lagging)
    }

    // -----------------------------------------------------------------
    // Time

    /// Settles what is due at `now`.
    pub fn tick(&mut self, now: Duration) -> Vec<Need> {
        let events = self.queries.tick(now);
        let needs = self.apply(events);
        self.input.tick(now);
        needs
    }

    /// When [`Exchange::tick`] or [`Exchange::flush_client`] has something to do.
    pub fn next_deadline(&self) -> Option<Duration> {
        [
            self.queries.next_deadline(),
            self.input.next_deadline(),
            self.client.as_ref().and_then(|c| c.flow.next_deadline()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Answers go to the program ahead of unstarted input.
    fn apply(&mut self, events: Vec<queries::Event>) -> Vec<Need> {
        let mut needs = Vec::new();
        for e in events {
            #[cfg(test)]
            self.settled.push(e.clone());
            match e {
                queries::Event::Answer { bytes, .. } => self.input.push_reply(bytes),
                queries::Event::Unanswered { .. } | queries::Event::Expired { .. } => {}
                queries::Event::ModelMissedDeadlines => needs.push(Need::FailModel),
            }
        }
        needs
    }
}

/// A line in `supervisor.log`; the unit tests keep quiet.
fn note(msg: &str) {
    if !cfg!(test) {
        log(msg);
    }
}

fn output_frame(bytes: &[u8]) -> Vec<u8> {
    proto::encode(kind::OUTPUT, bytes).expect("output frames are cut under the cap")
}

/// Queues the bytes the feeder holds back as live output. An unfinished
/// sequence can be up to 16 MiB, so it goes in frames under the cap.
fn push_held(flow: &mut ClientFlow, held: &[u8], now: Duration) {
    for c in held.chunks(MAX_PAYLOAD) {
        flow.push_live(Frame::new(output_frame(c)), now);
    }
}
