// SPDX-License-Identifier: GPL-3.0-only

//! Who answers each terminal query the program writes, and what it answers.
//!
//! At most one party answers a query: the supervisor or the attached
//! client's terminal. The tracker reports each query as the supervisor reads
//! it, and [`Queries::query`] decides its owner then. The supervisor owns
//! every query while no client is live or the client is lagging. Ownership
//! passes to a client at [`Queries::snapshot_requested`].
//!
//! Pure code: the caller passes the time as a [`Duration`] from any fixed
//! origin, calls [`Queries::tick`] at [`Queries::next_deadline`], and writes
//! each [`Event::Answer`] to the program ahead of queued input.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

/// How long the supervisor waits for the model's reply to a DSR 6 it owns,
/// counted from when the tracker saw the query.
pub const MODEL_REPLY_DEADLINE: Duration = Duration::from_millis(500);
/// How long a query forwarded to the client stays outstanding after the
/// client has received it.
pub const CLIENT_REPLY_EXPIRY: Duration = Duration::from_secs(2);
/// Missed DSR 6 deadlines within [`MISSED_WINDOW`] that count as a model
/// failure, like a missed snapshot deadline.
pub const MISSED_LIMIT: usize = 3;
/// See [`MISSED_LIMIT`].
pub const MISSED_WINDOW: Duration = Duration::from_secs(60);

/// A query, named by the number of the model frame that ends with it. The
/// supervisor cuts model frames at the last byte of every query, so each
/// frame number names at most one query, and numbers grow in output order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryId(pub u64);

/// The queries Perch answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `ESC [5n`, answered `CSI 0 n`.
    Dsr5,
    /// `ESC [6n`, answered `CSI <row>;<col> R`.
    Dsr6,
    /// `ESC [c`, answered `CSI ? ... c`.
    Da1,
    /// `ESC [>c`, answered `CSI > ... c`.
    Da2,
}

impl Kind {
    /// The supervisor's reply when it owns the query. DSR 6 has none: its
    /// reply comes from the model.
    pub fn fixed_reply(self) -> Option<&'static [u8]> {
        match self {
            Kind::Dsr5 => Some(b"\x1b[0n"),
            Kind::Da1 => Some(b"\x1b[?1;2c"),
            Kind::Da2 => Some(b"\x1b[>0;0;0c"),
            Kind::Dsr6 => None,
        }
    }
}

/// Whether the model can answer DSR 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelState {
    Ok,
    /// Restarted, and so wrong about the cursor until the program sends RIS.
    Partial,
    Failed,
}

/// Why a query the supervisor owns gets no reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoReply {
    /// A DSR 6 seen while the model was failed or partial, or still waiting
    /// when it failed.
    ModelUnavailable,
    /// A DSR 6 whose model reply did not arrive within
    /// [`MODEL_REPLY_DEADLINE`].
    ModelMissedDeadline,
}

/// What the caller must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Write `bytes` to the program ahead of every unstarted input item.
    Answer { id: QueryId, bytes: Vec<u8> },
    /// The supervisor owns this query and will not answer it.
    Unanswered { id: QueryId, why: NoReply },
    /// The client did not answer this forwarded query within
    /// [`CLIENT_REPLY_EXPIRY`]; it is dropped without an answer.
    Expired { id: QueryId },
    /// [`MISSED_LIMIT`] DSR 6 deadlines missed within [`MISSED_WINDOW`]:
    /// treat the model as failed.
    ModelMissedDeadlines,
}

/// The input side's view of the outstanding entries, used to recognize the
/// client's replies in its input.
pub trait ReplyMatcher {
    /// Whether any query forwarded to the client is still outstanding.
    fn outstanding(&self) -> bool;
    /// Called for each complete reply-shaped sequence in client input.
    /// Removes the oldest outstanding entry of `kind` and returns whether
    /// there was one.
    fn match_reply(&mut self, kind: Kind) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// Forwarded to the client; `delivered` is when its last byte was
    /// written to the client's socket, which starts the expiry clock.
    Client {
        delivered: Option<Duration>,
    },
    Supervisor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Model {
    /// DSR 5, DA1 and DA2 need nothing from the model.
    NotNeeded,
    /// No reply will come: the model was failed or partial.
    Unavailable,
    Awaiting {
        deadline: Duration,
    },
    Reply(Vec<u8>),
    Missed,
}

#[derive(Debug)]
struct Entry {
    kind: Kind,
    owner: Owner,
    model: Model,
}

/// The query state of one session.
#[derive(Debug)]
pub struct Queries {
    entries: BTreeMap<QueryId, Entry>,
    client_owns: bool,
    model: ModelState,
    misses: VecDeque<Duration>,
}

impl Default for Queries {
    fn default() -> Self {
        Queries::new()
    }
}

impl Queries {
    /// No client, and a complete model.
    pub fn new() -> Queries {
        Queries {
            entries: BTreeMap::new(),
            client_owns: false,
            model: ModelState::Ok,
            misses: VecDeque::new(),
        }
    }

    /// The tracker saw query `id` in output read now. Its owner is decided
    /// here: the client if one is live and past its snapshot request,
    /// otherwise the supervisor. A query the client owns must be forwarded
    /// in a frame that reports it through [`Queries::delivered`] or
    /// [`Queries::stalled`].
    pub fn query(&mut self, id: QueryId, kind: Kind, now: Duration) -> Vec<Event> {
        let model = match (kind, self.model) {
            (Kind::Dsr6, ModelState::Ok) => Model::Awaiting {
                deadline: now + MODEL_REPLY_DEADLINE,
            },
            (Kind::Dsr6, _) => Model::Unavailable,
            _ => Model::NotNeeded,
        };
        let owner = if self.client_owns {
            Owner::Client { delivered: None }
        } else {
            Owner::Supervisor
        };
        let old = self.entries.insert(id, Entry { kind, owner, model });
        debug_assert!(old.is_none(), "query {id:?} reported twice");
        self.release()
    }

    /// The model's DSR 6 reply for query `id`. Replies after the deadline,
    /// for unknown queries, or for queries already settled are discarded.
    /// The caller has already discarded replies from older generations.
    pub fn model_reply(&mut self, id: QueryId, bytes: &[u8], now: Duration) -> Vec<Event> {
        if let Some(entry) = self.entries.get_mut(&id)
            && let Model::Awaiting { deadline } = entry.model
            && now < deadline
        {
            entry.model = Model::Reply(bytes.to_vec());
            return self.release();
        }
        Vec::new()
    }

    /// The model's state changed. A failed or partial model answers no DSR
    /// 6: queries still waiting on it go unanswered, and do not count as
    /// missed deadlines. Replies it gave while complete stay valid.
    pub fn set_model(&mut self, state: ModelState) -> Vec<Event> {
        self.model = state;
        if state == ModelState::Ok {
            return Vec::new();
        }
        self.misses.clear();
        for entry in self.entries.values_mut() {
            if matches!(entry.model, Model::Awaiting { .. }) {
                entry.model = Model::Unavailable;
            }
        }
        self.release()
    }

    /// The supervisor requested the live client's snapshot. Queries read
    /// from now on belong to the client.
    pub fn snapshot_requested(&mut self) {
        self.client_owns = true;
    }

    /// The last bytes of these forwarded queries reached the client's
    /// socket. Their expiry clocks start now.
    pub fn delivered(&mut self, ids: &[QueryId], now: Duration) {
        for id in ids {
            if let Some(Entry {
                owner: Owner::Client { delivered },
                ..
            }) = self.entries.get_mut(id)
                && delivered.is_none()
            {
                *delivered = Some(now);
            }
        }
    }

    /// The client stalled and is lagging. The queries in `dropped` never
    /// reached it, so the supervisor answers them. Queries read from now on
    /// belong to the supervisor until the next snapshot request.
    pub fn stalled(&mut self, dropped: &[QueryId], now: Duration) -> Vec<Event> {
        self.client_owns = false;
        let missed = dropped.iter().filter(|id| self.hand_over(**id)).count();
        self.settle(missed, now)
    }

    /// The client is detaching or being taken over. The caller has already
    /// read what the client sent and passed it through the input side, and
    /// discards its input from now on. The supervisor answers every query
    /// still outstanding.
    pub fn client_left(&mut self, now: Duration) -> Vec<Event> {
        self.client_owns = false;
        let ids: Vec<QueryId> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.owner, Owner::Client { .. }))
            .map(|(id, _)| *id)
            .collect();
        let missed = ids.into_iter().filter(|id| self.hand_over(*id)).count();
        self.settle(missed, now)
    }

    /// Settles what is due at `now`: DSR 6 model deadlines and forwarded
    /// queries the client has not answered in time.
    pub fn tick(&mut self, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        let mut missed = 0;
        self.entries.retain(|id, entry| {
            if let Model::Awaiting { deadline } = entry.model
                && now >= deadline
            {
                entry.model = Model::Missed;
                if entry.owner == Owner::Supervisor {
                    missed += 1;
                }
            }
            if let Owner::Client {
                delivered: Some(at),
            } = entry.owner
                && now >= at + CLIENT_REPLY_EXPIRY
            {
                events.push(Event::Expired { id: *id });
                return false;
            }
            true
        });
        events.extend(self.settle(missed, now));
        events
    }

    /// The earliest time [`Queries::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.entries
            .values()
            .filter_map(|e| {
                let model = match e.model {
                    Model::Awaiting { deadline } => Some(deadline),
                    _ => None,
                };
                let expiry = match e.owner {
                    Owner::Client {
                        delivered: Some(at),
                    } => Some(at + CLIENT_REPLY_EXPIRY),
                    _ => None,
                };
                model.into_iter().chain(expiry).min()
            })
            .min()
    }

    /// Whether queries read now go to the client.
    pub fn client_owns(&self) -> bool {
        self.client_owns
    }

    /// Whether every query is settled.
    pub fn is_idle(&self) -> bool {
        self.entries.is_empty()
    }

    /// Makes a client's query the supervisor's. Returns whether the model
    /// had already missed its reply, which counts against the model as if
    /// the supervisor had owned the query all along.
    fn hand_over(&mut self, id: QueryId) -> bool {
        match self.entries.get_mut(&id) {
            Some(entry) if matches!(entry.owner, Owner::Client { .. }) => {
                entry.owner = Owner::Supervisor;
                entry.model == Model::Missed
            }
            _ => false,
        }
    }

    fn settle(&mut self, missed: usize, now: Duration) -> Vec<Event> {
        let mut events = self.release();
        for _ in 0..missed {
            self.count_miss(now, &mut events);
        }
        events
    }

    fn count_miss(&mut self, now: Duration, events: &mut Vec<Event>) {
        while self
            .misses
            .front()
            .is_some_and(|t| now.saturating_sub(*t) > MISSED_WINDOW)
        {
            self.misses.pop_front();
        }
        self.misses.push_back(now);
        if self.misses.len() >= MISSED_LIMIT {
            self.misses.clear();
            events.push(Event::ModelMissedDeadlines);
        }
    }

    /// Emits settled replies of the supervisor's queries in query order, so
    /// the program reads them in the order it asked, as from a terminal: a
    /// DSR 6 waiting for the model holds back later answers, at most until
    /// its deadline.
    fn release(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut done = Vec::new();
        for (id, entry) in &self.entries {
            if entry.owner != Owner::Supervisor {
                continue;
            }
            let event = match (&entry.model, entry.kind.fixed_reply()) {
                (Model::NotNeeded, Some(fixed)) => Event::Answer {
                    id: *id,
                    bytes: fixed.to_vec(),
                },
                (Model::Reply(bytes), _) => Event::Answer {
                    id: *id,
                    bytes: bytes.clone(),
                },
                (Model::Unavailable, _) => Event::Unanswered {
                    id: *id,
                    why: NoReply::ModelUnavailable,
                },
                (Model::Missed, _) => Event::Unanswered {
                    id: *id,
                    why: NoReply::ModelMissedDeadline,
                },
                (Model::Awaiting { .. }, _) => break,
                (Model::NotNeeded, None) => unreachable!("DSR 6 always involves the model"),
            };
            done.push(*id);
            events.push(event);
        }
        for id in done {
            self.entries.remove(&id);
        }
        events
    }
}

impl ReplyMatcher for Queries {
    fn outstanding(&self) -> bool {
        self.entries
            .values()
            .any(|e| matches!(e.owner, Owner::Client { .. }))
    }

    fn match_reply(&mut self, kind: Kind) -> bool {
        let found = self
            .entries
            .iter()
            .find(|(_, e)| e.kind == kind && matches!(e.owner, Owner::Client { .. }))
            .map(|(id, _)| *id);
        match found {
            Some(id) => {
                self.entries.remove(&id);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);
    const T0: Duration = Duration::from_secs(100);

    fn answer(id: u64, bytes: &[u8]) -> Event {
        Event::Answer {
            id: QueryId(id),
            bytes: bytes.to_vec(),
        }
    }

    fn unanswered(id: u64, why: NoReply) -> Event {
        Event::Unanswered {
            id: QueryId(id),
            why,
        }
    }

    #[test]
    fn detached_fixed_queries_get_fixed_replies() {
        let mut q = Queries::new();
        assert_eq!(q.query(QueryId(1), Kind::Dsr5, T0), [answer(1, b"\x1b[0n")]);
        assert_eq!(
            q.query(QueryId(2), Kind::Da1, T0),
            [answer(2, b"\x1b[?1;2c")]
        );
        assert_eq!(
            q.query(QueryId(3), Kind::Da2, T0),
            [answer(3, b"\x1b[>0;0;0c")]
        );
        assert!(q.is_idle());
    }

    #[test]
    fn detached_dsr6_takes_the_model_reply_within_500_ms() {
        let mut q = Queries::new();
        assert_eq!(q.query(QueryId(4), Kind::Dsr6, T0), []);
        assert_eq!(q.next_deadline(), Some(T0 + 500 * MS));
        assert_eq!(
            q.model_reply(QueryId(4), b"\x1b[2;1R", T0 + 499 * MS),
            [answer(4, b"\x1b[2;1R")]
        );
        assert!(q.is_idle());
        // A second reply for the same query is discarded.
        assert_eq!(q.model_reply(QueryId(4), b"\x1b[2;1R", T0 + 499 * MS), []);
    }

    #[test]
    fn a_model_reply_after_its_deadline_is_discarded() {
        let mut q = Queries::new();
        q.query(QueryId(4), Kind::Dsr6, T0);
        assert_eq!(
            q.tick(T0 + 500 * MS),
            [unanswered(4, NoReply::ModelMissedDeadline)]
        );
        assert_eq!(q.model_reply(QueryId(4), b"\x1b[2;1R", T0 + 501 * MS), []);
        // Even one that arrives before the tick that notices the deadline.
        q.query(QueryId(5), Kind::Dsr6, T0);
        assert_eq!(q.model_reply(QueryId(5), b"\x1b[2;1R", T0 + 500 * MS), []);
        assert_eq!(
            q.tick(T0 + 500 * MS),
            [unanswered(5, NoReply::ModelMissedDeadline)]
        );
    }

    #[test]
    fn answers_are_released_in_query_order() {
        let mut q = Queries::new();
        q.query(QueryId(1), Kind::Dsr6, T0);
        // The DA1 sentinel must not overtake the cursor report.
        assert_eq!(q.query(QueryId(2), Kind::Da1, T0), []);
        assert_eq!(
            q.model_reply(QueryId(1), b"\x1b[5;7R", T0 + 3 * MS),
            [answer(1, b"\x1b[5;7R"), answer(2, b"\x1b[?1;2c")]
        );
        q.query(QueryId(3), Kind::Dsr6, T0);
        q.query(QueryId(4), Kind::Dsr5, T0);
        assert_eq!(
            q.tick(T0 + 500 * MS),
            [
                unanswered(3, NoReply::ModelMissedDeadline),
                answer(4, b"\x1b[0n")
            ]
        );
    }

    #[test]
    fn three_missed_deadlines_within_a_minute_fail_the_model() {
        let mut q = Queries::new();
        q.query(QueryId(1), Kind::Dsr6, T0);
        q.tick(T0 + 500 * MS);
        q.query(QueryId(2), Kind::Dsr6, T0 + 30_000 * MS);
        q.tick(T0 + 30_500 * MS);
        // Exactly a minute after the first still counts.
        q.query(QueryId(3), Kind::Dsr6, T0 + 60_000 * MS);
        assert_eq!(
            q.tick(T0 + 60_500 * MS),
            [
                unanswered(3, NoReply::ModelMissedDeadline),
                Event::ModelMissedDeadlines
            ]
        );
    }

    #[test]
    fn missed_deadlines_spread_over_more_than_a_minute_do_not() {
        let mut q = Queries::new();
        for (id, at) in [(1, 0), (2, 30_000), (3, 60_001), (4, 90_002)] {
            q.query(QueryId(id), Kind::Dsr6, T0 + at * MS);
            let events = q.tick(T0 + (at + 500) * MS);
            assert_eq!(events, [unanswered(id, NoReply::ModelMissedDeadline)]);
        }
    }

    #[test]
    fn a_failed_or_partial_model_answers_no_dsr6_and_counts_no_miss() {
        let mut q = Queries::new();
        assert_eq!(q.set_model(ModelState::Partial), []);
        for id in 1..=5 {
            assert_eq!(
                q.query(QueryId(id), Kind::Dsr6, T0),
                [unanswered(id, NoReply::ModelUnavailable)]
            );
        }
        // A reply from the partial model is not used.
        assert_eq!(q.model_reply(QueryId(5), b"\x1b[1;1R", T0), []);
        // DSR 5 and DA are still answered.
        assert_eq!(q.query(QueryId(6), Kind::Dsr5, T0), [answer(6, b"\x1b[0n")]);
        q.set_model(ModelState::Ok);
        q.query(QueryId(7), Kind::Dsr6, T0);
        q.query(QueryId(8), Kind::Da2, T0);
        // The model fails while a reply is due.
        assert_eq!(
            q.set_model(ModelState::Failed),
            [
                unanswered(7, NoReply::ModelUnavailable),
                answer(8, b"\x1b[>0;0;0c")
            ]
        );
    }

    #[test]
    fn ownership_passes_at_the_snapshot_request() {
        let mut q = Queries::new();
        q.query(QueryId(1), Kind::Dsr6, T0);
        q.snapshot_requested();
        assert_eq!(q.query(QueryId(2), Kind::Da1, T0), []);
        assert!(q.outstanding());
        // The supervisor still answers the query it owned.
        assert_eq!(
            q.model_reply(QueryId(1), b"\x1b[1;1R", T0 + MS),
            [answer(1, b"\x1b[1;1R")]
        );
        // The client answers its own.
        assert!(q.match_reply(Kind::Da1));
        assert!(!q.outstanding());
        assert!(q.is_idle());
        assert_eq!(q.client_left(T0 + 2 * MS), []);
    }

    #[test]
    fn matching_takes_the_oldest_entry_of_the_kind() {
        let mut q = Queries::new();
        q.snapshot_requested();
        q.query(QueryId(1), Kind::Dsr6, T0);
        q.query(QueryId(2), Kind::Dsr5, T0);
        q.query(QueryId(3), Kind::Dsr6, T0);
        q.model_reply(QueryId(1), b"\x1b[1;1R", T0);
        q.model_reply(QueryId(3), b"\x1b[3;3R", T0);
        assert!(q.match_reply(Kind::Dsr6));
        assert!(!q.match_reply(Kind::Da2));
        assert_eq!(
            q.client_left(T0 + MS),
            [answer(2, b"\x1b[0n"), answer(3, b"\x1b[3;3R")]
        );
    }

    #[test]
    fn an_unmatched_entry_expires_two_seconds_after_delivery() {
        let mut q = Queries::new();
        q.snapshot_requested();
        q.query(QueryId(1), Kind::Da1, T0);
        // Not delivered yet: no clock runs.
        assert_eq!(q.next_deadline(), None);
        assert_eq!(q.tick(T0 + 10_000 * MS), []);
        q.delivered(&[QueryId(1)], T0 + 10_000 * MS);
        assert_eq!(q.next_deadline(), Some(T0 + 12_000 * MS));
        assert_eq!(q.tick(T0 + 11_999 * MS), []);
        assert_eq!(
            q.tick(T0 + 12_000 * MS),
            [Event::Expired { id: QueryId(1) }]
        );
        // Gone: a detach answers nothing, and a late reply matches nothing.
        assert_eq!(q.client_left(T0 + 13_000 * MS), []);
        assert!(!q.match_reply(Kind::Da1));
    }

    #[test]
    fn dropped_queries_are_answered_by_the_supervisor() {
        let mut q = Queries::new();
        q.snapshot_requested();
        q.query(QueryId(1), Kind::Dsr6, T0);
        q.query(QueryId(2), Kind::Da1, T0);
        q.query(QueryId(3), Kind::Dsr5, T0);
        q.model_reply(QueryId(1), b"\x1b[9;9R", T0 + MS);
        q.delivered(&[QueryId(3)], T0 + MS);
        // A client-owned DSR 6 past its model deadline counts no miss.
        assert_eq!(q.tick(T0 + 500 * MS), []);
        assert_eq!(
            q.stalled(&[QueryId(1), QueryId(2)], T0 + 5_000 * MS),
            [answer(1, b"\x1b[9;9R"), answer(2, b"\x1b[?1;2c")]
        );
        // The delivered one stays the client's, and queries read while it
        // lags are the supervisor's.
        assert!(q.outstanding());
        assert_eq!(
            q.query(QueryId(4), Kind::Da2, T0 + 5_001 * MS),
            [answer(4, b"\x1b[>0;0;0c")]
        );
    }

    #[test]
    fn a_dropped_dsr6_waits_for_the_model_and_a_late_one_goes_unanswered() {
        let mut q = Queries::new();
        q.snapshot_requested();
        q.query(QueryId(1), Kind::Dsr6, T0);
        q.query(QueryId(2), Kind::Dsr6, T0);
        assert_eq!(q.stalled(&[QueryId(1)], T0 + 100 * MS), []);
        assert_eq!(
            q.model_reply(QueryId(1), b"\x1b[4;4R", T0 + 200 * MS),
            [answer(1, b"\x1b[4;4R")]
        );
        assert_eq!(q.tick(T0 + 500 * MS), []);
        assert_eq!(
            q.client_left(T0 + 600 * MS),
            [unanswered(2, NoReply::ModelMissedDeadline)]
        );
    }

    #[test]
    fn a_detach_answers_every_outstanding_query() {
        let mut q = Queries::new();
        q.snapshot_requested();
        q.query(QueryId(1), Kind::Dsr5, T0);
        q.query(QueryId(2), Kind::Dsr6, T0);
        q.query(QueryId(3), Kind::Da2, T0);
        q.model_reply(QueryId(2), b"\x1b[2;2R", T0);
        q.delivered(&[QueryId(1), QueryId(2)], T0);
        assert_eq!(
            q.client_left(T0 + 1_999 * MS),
            [
                answer(1, b"\x1b[0n"),
                answer(2, b"\x1b[2;2R"),
                answer(3, b"\x1b[>0;0;0c")
            ]
        );
        assert!(!q.outstanding());
        // Queries after the detach are the supervisor's until the next
        // client's snapshot request.
        assert_eq!(q.query(QueryId(4), Kind::Dsr5, T0), [answer(4, b"\x1b[0n")]);
        q.snapshot_requested();
        assert_eq!(q.query(QueryId(5), Kind::Dsr5, T0), []);
    }
}
