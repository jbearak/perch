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
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;

use serde_json::json;

use super::log::log;
use super::{Conn, Phase, Supervisor, read_conn, send_error};
use crate::exchange::{self, ClientIo, Need};
use crate::meta::Exit;
use crate::proto::{
    self, Attach, Attached, DetachReason, Detached, ErrorBody, Exited, RestoreMode, kind,
};
use crate::restore::{self, Mode, Restore};
use crate::transport::Transport;
use crate::{clock, env_links, sys};

/// The live client's connection and terminal; the exchange keeps its flow.
pub(super) struct Peer {
    pub(super) conn: Conn,
    pub(super) label: String,
    pub(super) pid: Option<i32>,
    /// The client's terminal, clamped.
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) scrollback: u32,
    /// A restore asked of the model and not yet enqueued.
    pending: Option<Pending>,
    /// An input frame waiting for room in the input queue. Nothing more is
    /// read from the client until it fits.
    waiting_input: Option<Vec<u8>>,
    /// Writing to the client's socket failed.
    write_failed: bool,
}

impl ClientIo for Peer {
    type Sock<'a> = Sock<'a>;

    /// Control frames go first; output frames are written only once none
    /// waits, and control frames are only queued while no output frame is
    /// partly written.
    fn socket(&mut self) -> Option<Sock<'_>> {
        let mut sock = Sock {
            stream: &self.conn.stream,
            failed: &mut self.write_failed,
        };
        while !self.conn.out.is_empty() {
            let n = sock.write(&self.conn.out);
            if n == 0 {
                break;
            }
            self.conn.out.drain(..n);
        }
        self.conn.out.is_empty().then_some(Sock {
            stream: &self.conn.stream,
            failed: &mut self.write_failed,
        })
    }

    fn failed(&self) -> bool {
        self.write_failed
    }
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
pub(super) struct Sock<'a> {
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
    /// The attached client's connection and terminal.
    pub(super) fn peer(&self) -> Option<&Peer> {
        self.exchange.client().map(|c| &c.io)
    }

    fn peer_mut(&mut self) -> Option<&mut Peer> {
        self.exchange.client_mut().map(|c| &mut c.io)
    }

    /// Acts on what the exchange could not do itself.
    pub(super) fn act(&mut self, needs: Vec<Need>) {
        for need in needs {
            match need {
                Need::FailModel => self
                    .model
                    .fail("it missed 3 DSR 6 deadlines within a minute"),
                Need::Resync => self.request_restore(Mode::Screen),
                Need::DetachTooSlow => {
                    log("the client stalled 3 times within 10 minutes: detaching it");
                    self.client_leaves(Some(DetachReason::TooSlow));
                }
                Need::ClientGone => {
                    log("cannot write to the client; it is gone");
                    self.client_leaves(None);
                }
            }
        }
    }

    /// An `Attach` frame on a connection that sent an attach `Hello`.
    pub(super) fn attach(&mut self, mut conn: Conn, req: Attach) {
        if self.exchange.client().is_some() {
            if req.if_detached {
                conn.out.extend(proto::encode_json(kind::BUSY, &json!({})));
                conn.closing = true;
                self.conns.push(conn);
                return;
            }
            self.client_leaves(Some(DetachReason::Takeover));
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
        self.relink(&req.env);
        let peer = Peer {
            label: conn.label.clone(),
            conn,
            pid: req.pid,
            cols,
            rows,
            scrollback: req.scrollback,
            pending: None,
            waiting_input: None,
            write_failed: false,
        };
        self.exchange.attach(peer, self.elapsed());

        // The creator's attach: the program starts now, at its size.
        if self.plan.is_some() {
            self.creator_deadline = None;
            self.set_size(cols, rows);
            if let Err(msg) = self.spawn_program() {
                self.start_failed_attached(msg, 1);
            }
            self.exchange.start_with_program();
            return;
        }
        match req.restore {
            RestoreMode::Full => self.request_restore(Mode::Full),
            RestoreMode::Screen => self.request_restore(Mode::Screen),
            RestoreMode::None => {
                let now = self.elapsed();
                self.exchange
                    .start_live(self.feeder.in_long_sequence(), self.feeder.held(), now);
                self.set_size(cols, rows);
            }
        }
    }

    /// Points each linked variable's link at the attaching client's value.
    fn relink(&mut self, client: &std::collections::BTreeMap<String, String>) {
        if self.meta.linked_env.is_empty() {
            return;
        }
        let targets = env_links::targets(&self.meta.linked_env, client);
        match env_links::point(self.dir.as_fd(), &targets) {
            Ok(()) => self.links = targets,
            Err(e) => log(&format!("cannot re-point the linked variables: {e}")),
        }
    }

    /// Asks for the client's restore and stops PTY reads until it is
    /// enqueued. Queries read from now on are the client's.
    pub(super) fn request_restore(&mut self, mode: Mode) {
        let held = self.feeder.held().to_vec();
        let paste = self.feeder.tracker().bracketed_paste();
        let Some(epoch) = self
            .exchange
            .request_restore(self.feeder.in_long_sequence())
        else {
            return;
        };
        let c = self.peer().expect("attached");
        let rc = restore::Client {
            cols: c.cols,
            rows: c.rows,
            scrollback_rows: c.scrollback,
        };
        let (cols, rows) = rc.size();
        match self.model.request_restore(mode, rc, paste) {
            Some(id) => {
                self.peer_mut().expect("attached").pending = Some(Pending {
                    epoch,
                    id,
                    held,
                    cols,
                    rows,
                });
            }
            None => self.enqueue_restore(epoch, restore::partial(rc, paste), held),
        }
    }

    /// Queues a restore ahead of live output, the held-back bytes after
    /// it, and resizes the model and the PTY to the restore's size.
    fn enqueue_restore(&mut self, epoch: u64, r: Restore, held: Vec<u8>) {
        let now = self.elapsed();
        if !self.exchange.restore_ready(epoch, &r.bytes, &held, now) {
            return;
        }
        let Restore { size, nudge, .. } = r;
        let c = self.peer().expect("attached");
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
        let Some(c) = self.peer_mut() else {
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
        let paste = self.feeder.tracker().bracketed_paste();
        let r = restore::from_model(bytes, fallback, rc, paste);
        self.enqueue_restore(p.epoch, r, p.held);
    }

    /// The model failed: a restore waiting on it is built without it.
    pub(super) fn model_failed_restore(&mut self) {
        if self.peer().is_some_and(|c| c.pending.is_some()) {
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
        let Some(c) = self.peer_mut() else {
            return;
        };
        c.cols = cols;
        c.rows = rows;
        // While a restore is built, its own size goes first.
        if c.pending.is_none() {
            self.set_size(cols, rows);
        }
    }

    /// The live client's socket: reads frames and acts on them.
    pub(super) fn client_readable(&mut self) {
        let Some(c) = self.peer_mut() else {
            return;
        };
        if c.waiting_input.is_none() {
            read_conn(&mut c.conn);
        }
        self.client_frames();
        let gone = self
            .peer()
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
            let room = self.exchange.input_room();
            let Some(c) = self.peer_mut() else {
                return;
            };
            if let Some(bytes) = c.waiting_input.take() {
                if bytes.len() > room {
                    c.waiting_input = Some(bytes);
                    return;
                }
                self.exchange.client_input(&bytes, now);
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
        let needs = self.exchange.flush_client(writable, self.elapsed());
        self.act(needs);
    }

    /// Whether the client's socket should be polled for writing.
    pub(super) fn client_wants_write(&self) -> bool {
        let queued = self.peer().is_some_and(|c| !c.conn.out.is_empty());
        self.exchange.client_wants_write(queued)
    }

    /// Whether the client's socket should be polled for reading.
    pub(super) fn client_wants_read(&self) -> bool {
        self.peer()
            .is_some_and(|c| !c.conn.eof && c.waiting_input.is_none())
    }

    /// The client stops being attached: after a detach or a takeover it is
    /// told why (`reason`), otherwise it has gone. What it already sent is
    /// read and passed on first; queries still outstanding are answered;
    /// later input from it is discarded.
    pub(super) fn client_leaves(&mut self, reason: Option<DetachReason>) {
        let Some(c) = self.peer_mut() else {
            return;
        };
        if !c.conn.dead {
            read_conn(&mut c.conn);
        }
        let mut input = c.waiting_input.take().into_iter().collect::<Vec<_>>();
        while let Ok(Some(f)) = c.conn.decoder.next_frame() {
            if f.kind == kind::INPUT {
                input.push(f.payload);
            }
        }
        let now = self.elapsed();
        let (c, needs) = self.exchange.client_leaves(input, now).expect("attached");
        log(&format!(
            "client {} left{}",
            c.io.label,
            reason.map_or_else(String::new, |r| format!(" ({r:?})"))
        ));
        if let Some(reason) = reason
            && !c.io.conn.dead
            && !c.io.write_failed
        {
            let last = proto::encode_json(kind::DETACHED, &Detached { reason });
            self.send_last(c, last);
        }
        self.act(needs);
    }

    /// A leaving client's last frame, after the rest of any frame partly
    /// written; then its connection closes.
    fn send_last(&mut self, c: exchange::Attached<Peer>, frame: Vec<u8>) {
        let rest = c.flow().started().map(<[u8]>::to_vec);
        let mut conn = c.io.conn;
        if let Some(mut out) = rest {
            out.append(&mut conn.out);
            conn.out = out;
        }
        conn.out.extend(frame);
        conn.phase = Phase::Leaving;
        conn.closing = true;
        self.conns.push(conn);
    }

    /// Whether the client has been sent everything it is due, or never
    /// will be: what the program's end waits for before `exited`.
    pub(super) fn client_drained(&self) -> bool {
        self.peer().is_none_or(|c| c.pending.is_none()) && self.exchange.client_drained()
    }

    /// The session ends with a client attached: it gets `exited`, after
    /// any frame partly written, and the directory is removed.
    pub(super) fn exited_to_client(&mut self, exit: &Exit, start_failure: Option<u8>) {
        let Some(c) = self.exchange.client_ends() else {
            return;
        };
        let last = proto::encode_json(
            kind::EXITED,
            &Exited {
                exit: exit.clone(),
                status: exit_status(exit, start_failure),
            },
        );
        self.send_last(c, last);
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
}
