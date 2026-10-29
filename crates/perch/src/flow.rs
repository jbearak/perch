// SPDX-License-Identifier: GPL-3.0-only

//! Output to the attached client: a queue of whole frames with
//! backpressure, stall detection, drop and resync, and restore delivery.
//!
//! A slow client slows the program down: while the live frames queued reach
//! [`LIVE_CAP`], [`ClientFlow::read_budget`] is 0 and the supervisor stops
//! reading the PTY. A client that accepts no bytes for [`STALL_AFTER`] while
//! frames wait is stalled: the frame partly written stays, every frame not
//! started is dropped, the client lags and a new epoch starts. When it
//! accepts bytes again it gets a resync. [`TOO_SLOW_STALLS`] stalls within
//! [`TOO_SLOW_WINDOW`] detach it.
//!
//! Pure code: the caller passes the time as a [`Duration`] from any fixed
//! origin, writes through a [`Transport`], and calls [`ClientFlow::tick`]
//! at [`ClientFlow::next_deadline`].

use std::collections::VecDeque;
use std::time::Duration;

use crate::queries::QueryId;
use crate::transport::Transport;

/// The most live output queued for one client. Restores do not count.
pub const LIVE_CAP: usize = 4 << 20;
/// How long a client with frames waiting may accept no bytes.
pub const STALL_AFTER: Duration = Duration::from_secs(5);
/// Stalls within [`TOO_SLOW_WINDOW`] that detach a client as `too_slow`.
pub const TOO_SLOW_STALLS: usize = 3;
/// See [`TOO_SLOW_STALLS`].
pub const TOO_SLOW_WINDOW: Duration = Duration::from_secs(600);

/// One frame as it goes on the wire, with the queries whose last byte it
/// carries. The supervisor cuts client frames where it cuts model frames,
/// so a frame holds at most one query, at its end.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    pub bytes: Vec<u8>,
    pub queries: Vec<QueryId>,
}

impl Frame {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Frame {
        Frame {
            bytes: bytes.into(),
            queries: Vec::new(),
        }
    }

    pub fn with_query(bytes: impl Into<Vec<u8>>, id: QueryId) -> Frame {
        Frame {
            bytes: bytes.into(),
            queries: vec![id],
        }
    }
}

/// Where the client is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Live passthrough.
    Live,
    /// A snapshot was requested and the restore is not yet enqueued. The
    /// supervisor reads nothing from the PTY.
    Snapshot,
    /// Restore frames are queued. The supervisor reads nothing from the PTY
    /// until the last is written.
    Restoring,
    /// Output was dropped after a stall. Live output is not queued; the
    /// client waits for a resync.
    Lagging,
}

/// What the caller must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The last bytes of these queries reached the client's socket.
    Delivered(Vec<QueryId>),
    /// The restore's last frame is written. PTY reads resume.
    RestoreDelivered,
    /// The client stalled. Frames not started were dropped, and the queries
    /// in them never reached the client.
    Stalled { dropped: Vec<QueryId> },
    /// The stall just reported was the client's third within ten minutes:
    /// detach it with the reason `too_slow`.
    TooSlow,
    /// A lagging client accepts bytes again and its queue is empty: request
    /// a `screen` snapshot for the resync.
    ResyncDue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Restore,
    Live,
}

#[derive(Debug)]
struct Queued {
    frame: Frame,
    kind: Kind,
}

/// The output side of one attached client.
#[derive(Debug)]
pub struct ClientFlow {
    queue: VecDeque<Queued>,
    /// Bytes of the front frame already written.
    written: usize,
    live_bytes: usize,
    restore_frames: usize,
    phase: Phase,
    epoch: u64,
    /// When the queue last became non-empty or the socket last took bytes.
    progress_at: Duration,
    stalls: VecDeque<Duration>,
    resync_reported: bool,
}

impl ClientFlow {
    /// A client just attached, live, with an empty queue. For a `full` or
    /// `screen` attach the caller requests a snapshot at once.
    pub fn new(now: Duration) -> ClientFlow {
        ClientFlow {
            queue: VecDeque::new(),
            written: 0,
            live_bytes: 0,
            restore_frames: 0,
            phase: Phase::Live,
            epoch: 0,
            progress_at: now,
            stalls: VecDeque::new(),
            resync_reported: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Starts at 0 and grows with every stall.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many bytes the supervisor may read from the PTY now: none from a
    /// snapshot request until the restore is written, room under the live
    /// cap while live, and no limit from this client while it lags.
    pub fn read_budget(&self) -> usize {
        match self.phase {
            Phase::Snapshot | Phase::Restoring => 0,
            Phase::Lagging => usize::MAX,
            Phase::Live => LIVE_CAP.saturating_sub(self.live_bytes),
        }
    }

    /// Whether the queue holds no frame that has not started, which is when
    /// a snapshot may be requested: on attach, and on [`Event::ResyncDue`].
    pub fn snapshot_ready(&self) -> bool {
        self.queue.len() <= usize::from(self.written > 0)
    }

    /// The supervisor asks the model for this client's snapshot and stops
    /// reading the PTY. Returns the epoch to pass to
    /// [`ClientFlow::restore_enqueued`].
    pub fn request_snapshot(&mut self) -> u64 {
        debug_assert!(
            self.snapshot_ready(),
            "snapshot requested over queued frames"
        );
        self.phase = Phase::Snapshot;
        self.resync_reported = false;
        self.epoch
    }

    /// The restore is built. Its frames go ahead of all live output not yet
    /// started. Returns false, queuing nothing, if the request was from an
    /// older epoch or no snapshot is pending.
    pub fn restore_enqueued(&mut self, epoch: u64, frames: Vec<Frame>, now: Duration) -> bool {
        if epoch != self.epoch || self.phase != Phase::Snapshot {
            return false;
        }
        let at = usize::from(self.written > 0);
        let frames: Vec<Frame> = frames.into_iter().filter(|f| !f.bytes.is_empty()).collect();
        let n = frames.len();
        if self.queue.is_empty() && n > 0 {
            self.progress_at = now;
        }
        for (i, frame) in frames.into_iter().enumerate() {
            let queued = Queued {
                frame,
                kind: Kind::Restore,
            };
            self.queue.insert(at + i, queued);
        }
        self.restore_frames += n;
        self.phase = if n == 0 {
            Phase::Live
        } else {
            Phase::Restoring
        };
        true
    }

    /// Queues live output. Returns false, queuing nothing, while the client
    /// lags; the supervisor owns the queries in that output.
    pub fn push_live(&mut self, frame: Frame, now: Duration) -> bool {
        if self.phase == Phase::Lagging {
            return false;
        }
        debug_assert!(frame.queries.is_empty() || !frame.bytes.is_empty());
        if frame.bytes.is_empty() {
            return true;
        }
        if self.queue.is_empty() {
            self.progress_at = now;
        }
        self.live_bytes += frame.bytes.len();
        self.queue.push_back(Queued {
            frame,
            kind: Kind::Live,
        });
        true
    }

    /// The bytes to write next: the rest of the front frame.
    pub fn pending(&self) -> Option<&[u8]> {
        self.queue.front().map(|q| &q.frame.bytes[self.written..])
    }

    /// The socket took `n` bytes of [`ClientFlow::pending`].
    pub fn wrote(&mut self, n: usize, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if n == 0 {
            return events;
        }
        self.progress_at = now;
        self.written += n;
        let front = self.queue.front().expect("wrote with nothing pending");
        assert!(
            self.written <= front.frame.bytes.len(),
            "wrote past the frame"
        );
        if self.written == front.frame.bytes.len() {
            self.complete_front(&mut events);
        }
        events
    }

    /// Writes as much as `socket` accepts.
    pub fn flush(&mut self, socket: &mut impl Transport, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(bytes) = self.pending() {
            let n = socket.write(bytes);
            if n == 0 {
                break;
            }
            events.extend(self.wrote(n, now));
        }
        events
    }

    /// The socket polled writable. A lagging client with nothing queued is
    /// due its resync.
    pub fn writable(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if self.phase == Phase::Lagging && self.queue.is_empty() {
            self.report_resync(&mut events);
        }
        events
    }

    /// Applies the stall rule at `now`.
    pub fn tick(&mut self, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if self.queue.is_empty() || now < self.progress_at + STALL_AFTER {
            return events;
        }
        let keep = usize::from(self.written > 0);
        let mut dropped = Vec::new();
        for q in self.queue.drain(keep..) {
            match q.kind {
                Kind::Live => self.live_bytes -= q.frame.bytes.len(),
                Kind::Restore => self.restore_frames -= 1,
            }
            dropped.extend(q.frame.queries);
        }
        self.phase = Phase::Lagging;
        self.epoch += 1;
        self.progress_at = now;
        self.resync_reported = false;
        events.push(Event::Stalled { dropped });
        while self
            .stalls
            .front()
            .is_some_and(|t| now.saturating_sub(*t) > TOO_SLOW_WINDOW)
        {
            self.stalls.pop_front();
        }
        self.stalls.push_back(now);
        if self.stalls.len() >= TOO_SLOW_STALLS {
            events.push(Event::TooSlow);
        }
        events
    }

    /// When [`ClientFlow::tick`] could find a stall.
    pub fn next_deadline(&self) -> Option<Duration> {
        (!self.queue.is_empty()).then_some(self.progress_at + STALL_AFTER)
    }

    /// Live bytes queued, counting whole frames.
    pub fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    fn complete_front(&mut self, events: &mut Vec<Event>) {
        let q = self.queue.pop_front().expect("a front frame");
        self.written = 0;
        match q.kind {
            Kind::Live => self.live_bytes -= q.frame.bytes.len(),
            Kind::Restore => self.restore_frames -= 1,
        }
        if !q.frame.queries.is_empty() {
            events.push(Event::Delivered(q.frame.queries));
        }
        if q.kind == Kind::Restore && self.restore_frames == 0 && self.phase == Phase::Restoring {
            self.phase = Phase::Live;
            events.push(Event::RestoreDelivered);
        }
        if self.phase == Phase::Lagging && self.queue.is_empty() {
            self.report_resync(events);
        }
    }

    fn report_resync(&mut self, events: &mut Vec<Event>) {
        if !self.resync_reported {
            self.resync_reported = true;
            events.push(Event::ResyncDue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Scripted;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn live(bytes: &[u8]) -> Frame {
        Frame::new(bytes)
    }

    #[test]
    fn frames_go_out_whole_and_in_order() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        let mut sock = Scripted::new();
        flow.push_live(live(b"hello "), Duration::ZERO);
        flow.push_live(Frame::with_query(b"\x1b[6n", QueryId(7)), Duration::ZERO);
        sock.allow(8);
        assert_eq!(flow.flush(&mut sock, MS), []);
        assert_eq!(sock.out, b"hello \x1b[");
        sock.open();
        assert_eq!(
            flow.flush(&mut sock, 2 * MS),
            [Event::Delivered(vec![QueryId(7)])]
        );
        assert_eq!(sock.take(), b"hello \x1b[6n");
        assert_eq!(flow.live_bytes(), 0);
    }

    #[test]
    fn a_full_live_queue_stops_pty_reads_until_the_client_catches_up() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        assert_eq!(flow.read_budget(), LIVE_CAP);
        flow.push_live(live(&vec![b'x'; LIVE_CAP - 100]), Duration::ZERO);
        assert_eq!(flow.read_budget(), 100);
        flow.push_live(live(&[b'y'; 100]), Duration::ZERO);
        assert_eq!(flow.read_budget(), 0);
        // A slow client keeps taking bytes, so it never stalls and nothing
        // is dropped, however long it takes.
        let mut sock = Scripted::new();
        for second in 1..=20 {
            let now = second * 4 * S;
            sock.allow(LIVE_CAP / 40);
            assert_eq!(flow.flush(&mut sock, now), []);
            assert_eq!(flow.tick(now + 4 * S), []);
            assert_eq!(flow.read_budget(), 0, "whole frames count until written");
        }
        sock.open();
        flow.flush(&mut sock, 90 * S);
        assert_eq!(sock.out.len(), LIVE_CAP);
        assert_eq!(flow.read_budget(), LIVE_CAP);
    }

    #[test]
    fn an_idle_client_with_nothing_pending_never_stalls() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        assert_eq!(flow.next_deadline(), None);
        assert_eq!(flow.tick(3600 * S), []);
        assert_eq!(flow.phase(), Phase::Live);
    }

    #[test]
    fn five_seconds_without_progress_drops_unstarted_frames_only() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        let mut sock = Scripted::new();
        flow.push_live(live(b"AAAA"), Duration::ZERO);
        flow.push_live(Frame::with_query(b"B\x1b[c", QueryId(1)), Duration::ZERO);
        flow.push_live(Frame::with_query(b"C\x1b[5n", QueryId(2)), Duration::ZERO);
        sock.allow(2);
        flow.flush(&mut sock, S);
        assert_eq!(flow.next_deadline(), Some(6 * S));
        assert_eq!(flow.tick(6 * S - MS), []);
        assert_eq!(
            flow.tick(6 * S),
            [Event::Stalled {
                dropped: vec![QueryId(1), QueryId(2)]
            }]
        );
        assert_eq!(flow.phase(), Phase::Lagging);
        assert_eq!(flow.epoch(), 1);
        assert_eq!(flow.live_bytes(), 4);
        // The program is no longer held up by this client.
        assert_eq!(flow.read_budget(), usize::MAX);
        assert!(!flow.push_live(live(b"discarded"), 7 * S));
        // The partly written frame finishes, framing intact, and the client
        // is then due a resync.
        sock.open();
        assert_eq!(flow.flush(&mut sock, 8 * S), [Event::ResyncDue]);
        assert_eq!(sock.take(), b"AAAA");
        assert_eq!(flow.writable(), [], "reported once");
    }

    #[test]
    fn a_stall_between_frames_waits_for_writable_before_the_resync() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        flow.push_live(live(b"AAAA"), Duration::ZERO);
        assert_eq!(flow.tick(5 * S), [Event::Stalled { dropped: vec![] }]);
        assert_eq!(flow.pending(), None);
        assert_eq!(flow.next_deadline(), None);
        assert_eq!(flow.writable(), [Event::ResyncDue]);
        let epoch = flow.request_snapshot();
        assert_eq!(epoch, 1);
        assert!(flow.restore_enqueued(epoch, vec![Frame::new(b"RESYNC")], 6 * S));
        let mut sock = Scripted::new();
        sock.open();
        assert_eq!(flow.flush(&mut sock, 6 * S), [Event::RestoreDelivered]);
        assert_eq!(sock.take(), b"RESYNC");
        assert_eq!(flow.phase(), Phase::Live);
    }

    #[test]
    fn pty_reads_pause_from_snapshot_request_until_the_restore_is_written() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        assert!(flow.snapshot_ready());
        let epoch = flow.request_snapshot();
        assert_eq!(flow.phase(), Phase::Snapshot);
        assert_eq!(flow.read_budget(), 0);
        // Nothing queued, so no stall while the model builds the snapshot.
        assert_eq!(flow.tick(60 * S), []);
        let restore = vec![Frame::new(b"\x18PROLOGUE"), Frame::new(b"SCREEN")];
        assert!(flow.restore_enqueued(epoch, restore, 61 * S));
        assert_eq!(flow.phase(), Phase::Restoring);
        assert_eq!(flow.read_budget(), 0);
        // The bytes the supervisor held back follow the restore.
        flow.push_live(live(b"\x1b[3"), 61 * S);
        let mut sock = Scripted::new();
        sock.allow(9);
        assert_eq!(flow.flush(&mut sock, 62 * S), []);
        assert_eq!(flow.read_budget(), 0);
        sock.open();
        assert_eq!(flow.flush(&mut sock, 63 * S), [Event::RestoreDelivered]);
        assert_eq!(sock.take(), b"\x18PROLOGUESCREEN\x1b[3");
        assert_eq!(flow.read_budget(), LIVE_CAP);
    }

    #[test]
    fn a_restore_goes_ahead_of_live_output_but_behind_a_started_frame() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        let mut sock = Scripted::new();
        flow.push_live(live(b"OLD!"), Duration::ZERO);
        sock.allow(1);
        flow.flush(&mut sock, Duration::ZERO);
        let epoch = flow.request_snapshot();
        flow.push_live(live(b"live"), Duration::ZERO);
        assert!(flow.restore_enqueued(epoch, vec![Frame::new(b"RESTORE")], Duration::ZERO));
        sock.open();
        flow.flush(&mut sock, MS);
        assert_eq!(sock.take(), b"OLD!RESTORElive");
    }

    #[test]
    fn a_stall_mid_restore_drops_the_rest_of_it_with_the_live_frames() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        let epoch = flow.request_snapshot();
        let restore = vec![
            Frame::new(b"R1R1"),
            Frame::new(b"R2R2"),
            Frame::new(b"R3R3"),
        ];
        flow.restore_enqueued(epoch, restore, Duration::ZERO);
        flow.push_live(live(b"held"), Duration::ZERO);
        let mut sock = Scripted::new();
        sock.allow(6);
        flow.flush(&mut sock, S);
        assert_eq!(flow.tick(6 * S), [Event::Stalled { dropped: vec![] }]);
        assert_eq!(flow.phase(), Phase::Lagging);
        assert_eq!(flow.read_budget(), usize::MAX);
        sock.open();
        assert_eq!(flow.flush(&mut sock, 7 * S), [Event::ResyncDue]);
        assert_eq!(sock.take(), b"R1R1R2R2");
        // The resync, a screen restore, follows.
        let epoch = flow.request_snapshot();
        assert!(flow.restore_enqueued(epoch, vec![Frame::new(b"SCREEN")], 7 * S));
        assert_eq!(flow.flush(&mut sock, 7 * S), [Event::RestoreDelivered]);
        assert_eq!(sock.take(), b"SCREEN");
    }

    #[test]
    fn a_restore_from_an_older_epoch_is_refused() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        let mut sock = Scripted::new();
        flow.push_live(live(b"AB"), Duration::ZERO);
        sock.allow(1);
        flow.flush(&mut sock, Duration::ZERO);
        let epoch = flow.request_snapshot();
        // The started frame stalls while the snapshot is being built.
        assert_eq!(flow.tick(5 * S), [Event::Stalled { dropped: vec![] }]);
        assert!(!flow.restore_enqueued(epoch, vec![Frame::new(b"STALE")], 5 * S));
        sock.open();
        assert_eq!(flow.flush(&mut sock, 6 * S), [Event::ResyncDue]);
        assert_eq!(sock.take(), b"AB");
    }

    fn stall(flow: &mut ClientFlow, at: Duration) -> Vec<Event> {
        let mut sock = Scripted::new();
        sock.open();
        flow.flush(&mut sock, at - STALL_AFTER);
        if flow.phase() == Phase::Lagging {
            flow.writable();
            let epoch = flow.request_snapshot();
            flow.restore_enqueued(epoch, vec![], at - STALL_AFTER);
        }
        flow.push_live(live(b"x"), at - STALL_AFTER);
        flow.tick(at)
    }

    #[test]
    fn three_stalls_within_ten_minutes_is_too_slow() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        assert_eq!(stall(&mut flow, 10 * S).len(), 1);
        assert_eq!(stall(&mut flow, 300 * S).len(), 1);
        let third = stall(&mut flow, 610 * S);
        assert_eq!(third, [Event::Stalled { dropped: vec![] }, Event::TooSlow]);
    }

    #[test]
    fn stalls_spread_over_more_than_ten_minutes_are_not() {
        let mut flow = ClientFlow::new(Duration::ZERO);
        assert_eq!(stall(&mut flow, 10 * S).len(), 1);
        assert_eq!(stall(&mut flow, 300 * S).len(), 1);
        assert_eq!(stall(&mut flow, 611 * S).len(), 1);
        // 600 seconds after the second stall is still within ten minutes.
        assert_eq!(stall(&mut flow, 901 * S).len(), 1);
        assert_eq!(stall(&mut flow, 905 * S).len(), 2);
    }
}
