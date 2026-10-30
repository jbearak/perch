// SPDX-License-Identifier: GPL-3.0-only

//! The attached client: one at a time, taken over by a new attach.
//!
//! A client's output goes through [`ClientFlow`]: whole frames, with
//! backpressure on the PTY, stalls, drops and resyncs. A restore is asked
//! of the model when the client attaches (or resyncs). From that request
//! until the restore is enqueued the supervisor reads nothing from the PTY,
//! and queries read after the request are the client's. The client gets the
//! restore, then the bytes the feeder still holds back, then live output,
//! cut where the model's frames are cut so each frame holds at most one
//! query, at its end.
//!
//! A client that attaches in the middle of a sequence over 16 MiB gets none
//! of the rest of it, which its terminal would print as text: its output is
//! withheld until the feeder reports where the sequence ends.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use serde_json::json;

use super::log::log;
use super::{Conn, Phase, Supervisor, read_conn, send_error};
use crate::flow::{self, ClientFlow, Frame};
use crate::meta::Exit;
use crate::proto::{self, Attach, Attached, Detached, ErrorBody, Exited, RestoreMode, kind};
use crate::queries::QueryId;
use crate::restore::{self, Mode, Nudge};
use crate::transport::Transport;
use crate::{clock, sys};

/// The most raw bytes in one output frame.
const MAX_PAYLOAD: usize = proto::MAX_FRAME - 1;

/// The live client.
pub(super) struct Client {
    pub(super) conn: Conn,
    pub(super) flow: ClientFlow,
    pub(super) label: String,
    pub(super) pid: Option<i32>,
    /// The client's terminal, clamped.
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) scrollback: u32,
    /// A restore asked of the model and not yet enqueued.
    pending: Option<Pending>,
    /// Output is withheld: the client attached in the middle of a long
    /// sequence, which has not ended yet.
    withhold: bool,
    /// An input frame waiting for room in the input queue. Nothing more is
    /// read from the client until it fits.
    waiting_input: Option<Vec<u8>>,
    /// Writing to the client's socket failed.
    write_failed: bool,
}

struct Pending {
    epoch: u64,
    /// The model's restore request.
    id: u64,
    held: Vec<u8>,
    cols: u16,
    rows: u16,
}

/// Writes to a client's socket without blocking.
struct Sock<'a> {
    stream: &'a UnixStream,
    failed: &'a mut bool,
}

impl Transport for Sock<'_> {
    fn write(&mut self, bytes: &[u8]) -> usize {
        match (&*self.stream).write(bytes) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                0
            }
            Err(_) => {
                *self.failed = true;
                0
            }
        }
    }
}

fn output_frame(bytes: &[u8]) -> Vec<u8> {
    proto::encode(kind::OUTPUT, bytes).expect("output frames are cut under the cap")
}

/// The restore's bytes as output frames.
fn restore_frames(bytes: &[u8]) -> Vec<Frame> {
    bytes
        .chunks(MAX_PAYLOAD)
        .map(|c| Frame::new(output_frame(c)))
        .collect()
}

/// Queues the bytes the feeder holds back as live output. An unfinished
/// sequence can be up to 16 MiB, so it goes in frames under the cap.
fn push_held(flow: &mut ClientFlow, held: &[u8], now: std::time::Duration) {
    for c in held.chunks(MAX_PAYLOAD) {
        flow.push_live(Frame::new(output_frame(c)), now);
    }
}

/// `perch attach`'s exit status for a program's end.
pub fn exit_status(exit: &Exit, start_failure: Option<u8>) -> u8 {
    if let Some(code) = start_failure {
        return code;
    }
    match (exit.code, exit.signal_number) {
        (Some(c), _) => u8::try_from(c & 0xff).unwrap_or(1),
        (None, Some(s)) => u8::try_from(128 + s).unwrap_or(1),
        _ => 1,
    }
}

impl Supervisor {
    /// How many bytes may be read from the PTY now.
    pub(super) fn read_budget(&self) -> usize {
        self.client
            .as_ref()
            .map_or(usize::MAX, |c| c.flow.read_budget())
    }

    /// An `Attach` frame on a connection that sent an attach `Hello`.
    pub(super) fn attach(&mut self, mut conn: Conn, req: Attach) {
        if self.client.is_some() {
            if req.if_detached {
                conn.out.extend(proto::encode_json(kind::BUSY, &json!({})));
                conn.closing = true;
                self.conns.push(conn);
                return;
            }
            self.client_leaves(Some("takeover"));
        }
        if self.killing.is_some() || self.ending.is_some() || self.program_status.is_some() {
            send_error(&mut conn, ErrorBody::new("ending", "the session is ending"));
            self.conns.push(conn);
            return;
        }
        let cols = req.cols.clamp(1, restore::MAX_CELLS);
        let rows = req.rows.clamp(1, restore::MAX_CELLS);
        conn.phase = Phase::Attached;
        conn.out
            .extend(proto::encode_json(kind::ATTACHED, &Attached { rows, cols }));
        log(&format!(
            "client attached ({}, restore {:?})",
            conn.label, req.restore
        ));
        self.client = Some(Client {
            label: conn.label.clone(),
            conn,
            flow: ClientFlow::new(self.elapsed()),
            pid: req.pid,
            cols,
            rows,
            scrollback: req.scrollback,
            pending: None,
            withhold: false,
            waiting_input: None,
            write_failed: false,
        });

        // The creator's attach: the program starts now, at its size.
        if self.plan.is_some() {
            self.creator_deadline = None;
            self.set_size(cols, rows);
            if let Err(msg) = self.spawn_program() {
                self.start_failed_attached(msg, 1);
            }
            self.queries.snapshot_requested();
            return;
        }
        match req.restore {
            RestoreMode::Full => self.request_restore(Mode::Full),
            RestoreMode::Screen => self.request_restore(Mode::Screen),
            RestoreMode::None => {
                self.queries.snapshot_requested();
                let withhold = self.feeder.in_long_sequence();
                let held = self.feeder.held().to_vec();
                let now = self.elapsed();
                let c = self.client.as_mut().expect("attached");
                c.withhold = withhold;
                if !withhold {
                    push_held(&mut c.flow, &held, now);
                }
                self.set_size(cols, rows);
            }
        }
    }

    /// Asks for the client's restore and stops PTY reads until it is
    /// enqueued. Queries read from now on are the client's.
    pub(super) fn request_restore(&mut self, mode: Mode) {
        let in_long = self.feeder.in_long_sequence();
        let held = self.feeder.held().to_vec();
        let paste = self.feeder.tracker().bracketed_paste();
        let Some(c) = self.client.as_mut() else {
            return;
        };
        let epoch = c.flow.request_snapshot();
        self.queries.snapshot_requested();
        c.withhold = in_long;
        let rc = restore::Client {
            cols: c.cols,
            rows: c.rows,
            scrollback_rows: c.scrollback,
        };
        let (cols, rows) = rc.size();
        match self.model.request_restore(mode, rc, paste) {
            Some(id) => {
                c.pending = Some(Pending {
                    epoch,
                    id,
                    held,
                    cols,
                    rows,
                });
            }
            None => {
                let r = restore::partial(rc, paste);
                self.enqueue_restore(epoch, &r.bytes, held, (cols, rows), r.nudge);
            }
        }
    }

    /// Queues a restore ahead of live output, the held-back bytes after
    /// it, and resizes the model and the PTY to the client's size.
    fn enqueue_restore(
        &mut self,
        epoch: u64,
        bytes: &[u8],
        held: Vec<u8>,
        size: (u16, u16),
        nudge: Option<Nudge>,
    ) {
        let now = self.elapsed();
        let Some(c) = self.client.as_mut() else {
            return;
        };
        if !c.flow.restore_enqueued(epoch, restore_frames(bytes), now) {
            return;
        }
        if !c.withhold {
            push_held(&mut c.flow, &held, now);
        }
        let (want_cols, want_rows) = (c.cols, c.rows);
        self.set_size(size.0, size.1);
        if let Some(n) = nudge {
            self.set_pty(n.via.0, n.via.1);
            self.set_pty(n.back.0, n.back.1);
        }
        // The client resized while the restore was being built.
        if (want_cols, want_rows) != size {
            self.set_size(want_cols, want_rows);
        }
    }

    /// The model answered a restore request, or failed while one waited.
    pub(super) fn model_restore(&mut self, id: Option<u64>, fallback: bool, bytes: Vec<u8>) {
        let Some(c) = self.client.as_mut() else {
            return;
        };
        let matches = match (&c.pending, id) {
            (Some(p), Some(id)) => p.id == id,
            (Some(_), None) => true,
            _ => false,
        };
        if !matches {
            return;
        }
        let p = c.pending.take().expect("matched");
        let rc = restore::Client {
            cols: p.cols,
            rows: p.rows,
            scrollback_rows: c.scrollback,
        };
        if id.is_none() || bytes.is_empty() {
            let paste = self.feeder.tracker().bracketed_paste();
            let r = restore::partial(rc, paste);
            // A failed model is not resized; a later one starts at this
            // size.
            self.model.resize(p.cols, p.rows);
            self.enqueue_restore(p.epoch, &r.bytes, p.held, (p.cols, p.rows), r.nudge);
        } else {
            let nudge = fallback.then(|| Nudge::for_size(p.cols, p.rows));
            self.enqueue_restore(p.epoch, &bytes, p.held, (p.cols, p.rows), nudge);
        }
    }

    /// The model failed: a restore waiting on it is built without it.
    pub(super) fn model_failed_restore(&mut self) {
        if self.client.as_ref().is_some_and(|c| c.pending.is_some()) {
            self.model_restore(None, true, Vec::new());
        }
    }

    /// Resizes the model and the PTY.
    pub(super) fn set_size(&mut self, cols: u16, rows: u16) {
        self.model.resize(cols, rows);
        self.set_pty(cols, rows);
    }

    fn set_pty(&mut self, cols: u16, rows: u16) {
        let size = sys::Winsize { rows, cols };
        if let Some(m) = &self.master
            && let Err(e) = sys::set_winsize(m.as_raw_fd(), size)
        {
            log(&format!("TIOCSWINSZ: {e}"));
        }
        self.size = size;
    }

    /// The client's terminal changed size.
    fn client_resized(&mut self, cols: u16, rows: u16) {
        let cols = cols.clamp(1, restore::MAX_CELLS);
        let rows = rows.clamp(1, restore::MAX_CELLS);
        let Some(c) = self.client.as_mut() else {
            return;
        };
        c.cols = cols;
        c.rows = rows;
        // While a restore is built, its own size goes first.
        if c.pending.is_none() {
            self.set_size(cols, rows);
        }
    }

    /// Output read from the PTY at `base`, for the client: cut at the end
    /// of each query (`cuts`), and nothing before `from` if the client was
    /// withholding a long sequence that ended there.
    pub(super) fn client_output(&mut self, chunk: &[u8], base: u64, cuts: &[(u64, QueryId)]) {
        let now = self.elapsed();
        let Some(c) = self.client.as_mut() else {
            return;
        };
        if c.withhold {
            return;
        }
        let mut pos = 0;
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

    /// The long sequence ended at `at`. A client withholding it gets the
    /// output after it, which is `chunk[at - base..]` of this read.
    pub(super) fn long_sequence_ended(&mut self, at: u64, base: u64, len: usize) -> Option<usize> {
        let c = self.client.as_mut()?;
        if !c.withhold {
            return None;
        }
        c.withhold = false;
        Some(
            usize::try_from(at - base)
                .expect("within the chunk")
                .min(len),
        )
    }

    /// The live client's socket: reads frames and acts on them.
    pub(super) fn client_readable(&mut self) {
        let Some(c) = self.client.as_mut() else {
            return;
        };
        if c.waiting_input.is_none() {
            read_conn(&mut c.conn);
        }
        self.client_frames();
        let gone = self
            .client
            .as_ref()
            .is_some_and(|c| c.conn.dead || (c.conn.eof && c.waiting_input.is_none()));
        if gone {
            log("client disconnected");
            self.client_leaves(None);
        }
    }

    /// Processes the client's decoded frames while input fits.
    fn client_frames(&mut self) {
        loop {
            let now = self.elapsed();
            let Some(c) = self.client.as_mut() else {
                return;
            };
            if let Some(bytes) = c.waiting_input.take() {
                if bytes.len() > self.input.room() {
                    c.waiting_input = Some(bytes);
                    return;
                }
                self.input.client_read(&bytes, now, &mut self.queries);
                self.last_input = Some(clock::unix_now());
                continue;
            }
            let f = match c.conn.decoder.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => return,
                Err(e) => {
                    log(&format!("client sent a bad frame: {e}"));
                    c.conn.dead = true;
                    return;
                }
            };
            match f.kind {
                kind::INPUT => c.waiting_input = Some(f.payload),
                kind::RESIZE => {
                    if let Ok(r) = serde_json::from_slice::<proto::Resize>(&f.payload) {
                        self.client_resized(r.cols, r.rows);
                    }
                }
                // Anything else is from a newer client; ignore it.
                _ => {}
            }
        }
    }

    /// Writes what the client is due and applies the stall rule.
    pub(super) fn client_flush(&mut self, writable: bool) {
        let now = self.elapsed();
        let Some(c) = self.client.as_mut() else {
            return;
        };
        let mut events = Vec::new();
        {
            let mut sock = Sock {
                stream: &c.conn.stream,
                failed: &mut c.write_failed,
            };
            // Control frames go first; they are only queued while no
            // output frame is partly written.
            while !c.conn.out.is_empty() {
                let n = sock.write(&c.conn.out);
                if n == 0 {
                    break;
                }
                c.conn.out.drain(..n);
            }
            if c.conn.out.is_empty() {
                events.extend(c.flow.flush(&mut sock, now));
            }
        }
        if writable && c.conn.out.is_empty() {
            events.extend(c.flow.writable());
        }
        events.extend(c.flow.tick(now));
        if c.write_failed {
            log("cannot write to the client; it is gone");
            self.client_leaves(None);
            return;
        }
        self.flow_events(events);
    }

    fn flow_events(&mut self, events: Vec<flow::Event>) {
        let now = self.elapsed();
        for e in events {
            match e {
                flow::Event::Delivered(ids) => self.queries.delivered(&ids, now),
                flow::Event::RestoreDelivered => {}
                flow::Event::Stalled { dropped } => {
                    log("the client stalled; output was dropped and it will be resynced");
                    let ev = self.queries.stalled(&dropped, now);
                    self.apply_queries(ev);
                }
                flow::Event::TooSlow => {
                    log("the client stalled 3 times within 10 minutes: detaching it");
                    self.client_leaves(Some("too_slow"));
                    return;
                }
                flow::Event::ResyncDue => self.request_restore(Mode::Screen),
            }
        }
    }

    /// Whether the client's socket should be polled for writing.
    pub(super) fn client_wants_write(&self) -> bool {
        self.client.as_ref().is_some_and(|c| {
            !c.conn.out.is_empty()
                || c.flow.pending().is_some()
                || c.flow.phase() == flow::Phase::Lagging
        })
    }

    /// Whether the client's socket should be polled for reading.
    pub(super) fn client_wants_read(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|c| !c.conn.eof && c.waiting_input.is_none())
    }

    /// The client stops being attached: after a detach or a takeover it is
    /// told why (`reason`), otherwise it has gone. What it already sent is
    /// read and passed on first; queries still outstanding are answered;
    /// later input from it is discarded.
    pub(super) fn client_leaves(&mut self, reason: Option<&str>) {
        let Some(mut c) = self.client.take() else {
            return;
        };
        let now = self.elapsed();
        if !c.conn.dead {
            read_conn(&mut c.conn);
        }
        let mut input = c.waiting_input.take().into_iter().collect::<Vec<_>>();
        while let Ok(Some(f)) = c.conn.decoder.next_frame() {
            if f.kind == kind::INPUT {
                input.push(f.payload);
            }
        }
        for bytes in input {
            let room = self.input.room();
            if bytes.len() > room {
                log(&format!(
                    "the input queue is full: {} bytes of a leaving client's input were dropped",
                    bytes.len() - room
                ));
            }
            self.input
                .client_read(&bytes[..bytes.len().min(room)], now, &mut self.queries);
        }
        self.input.client_left();
        let events = self.queries.client_left(now);
        self.apply_queries(events);
        log(&format!(
            "client {} left{}",
            c.label,
            reason.map_or_else(String::new, |r| format!(" ({r})"))
        ));
        let Some(reason) = reason else {
            return;
        };
        if c.conn.dead || c.write_failed {
            return;
        }
        let mut conn = c.conn;
        if let Some(rest) = c.flow.started() {
            let mut out = rest.to_vec();
            out.append(&mut conn.out);
            conn.out = out;
        }
        conn.out.extend(proto::encode_json(
            kind::DETACHED,
            &Detached {
                reason: reason.to_owned(),
            },
        ));
        conn.phase = Phase::Leaving;
        conn.closing = true;
        self.conns.push(conn);
    }

    /// Whether the client has been sent everything it is due, or never
    /// will be: what the program's end waits for before `exited`.
    pub(super) fn client_drained(&self) -> bool {
        self.client.as_ref().is_none_or(|c| {
            c.pending.is_none()
                && (c.flow.pending().is_none() || c.flow.phase() == flow::Phase::Lagging)
        })
    }

    /// The session ends with a client attached: it gets `exited`, after
    /// any frame partly written, and the directory is removed.
    pub(super) fn exited_to_client(&mut self, exit: &Exit, start_failure: Option<u8>) {
        let Some(c) = self.client.take() else {
            return;
        };
        let mut conn = c.conn;
        if let Some(rest) = c.flow.started() {
            let mut out = rest.to_vec();
            out.append(&mut conn.out);
            conn.out = out;
        }
        conn.out.extend(proto::encode_json(
            kind::EXITED,
            &Exited {
                exit: exit.clone(),
                status: exit_status(exit, start_failure),
            },
        ));
        conn.phase = Phase::Leaving;
        conn.closing = true;
        self.conns.push(conn);
    }

    /// The program did not start for the attached creator: it hears why,
    /// and nothing is left behind.
    pub(super) fn start_failed_attached(&mut self, msg: String, code: u8) {
        log(&msg);
        let exit = Exit {
            error: Some(msg),
            ..Exit::default()
        };
        self.exited_to_client(&exit, Some(code));
        self.start_failure = Some(code);
    }

    /// When the client's flow has something to do.
    pub(super) fn client_deadline(&self) -> Option<Instant> {
        let c = self.client.as_ref()?;
        c.flow.next_deadline().map(|d| self.epoch + d)
    }
}
