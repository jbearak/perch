// SPDX-License-Identifier: GPL-3.0-only

//! The terminal model and the supervisor's side of it.
//!
//! Ghostty's library is Zig built `ReleaseSafe`: a bug in it is an abort,
//! which Rust cannot catch. So the model runs in a process of its own
//! ([`process`]), forked from the supervisor, and a model bug never costs
//! the user their program.
//!
//! [`Model`] here is the supervisor's handle. It sends frames ([`feed`]
//! decides what they hold), reads DSR 6 replies, OSC 7 changes and
//! snapshots, and decides when the model has failed: its process died, it
//! fell more than [`BACKLOG_LIMIT`] behind, it missed a snapshot deadline,
//! its memory passed [`memory_limit`] (macOS; Linux enforces `RLIMIT_AS`
//! in the model itself), or the supervisor says so after missed DSR 6
//! deadlines. A failed model is killed and passthrough carries on. A fresh
//! one is started at most [`RESTART_LIMIT`] times per [`RESTART_WINDOW`],
//! from a point where the stream is between sequences, and is partial: it
//! has not seen earlier output, so it answers no DSR 6 until the program
//! resets the terminal with RIS.
//!
//! Each model process has a generation, and everything it says carries it.
//! Anything from an older generation is discarded.

pub mod feed;
mod process;
pub mod wire;

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use self::wire::{Decoder, FromModel, ToModel};
use crate::meta::Exit;
use crate::queries::ModelState;
use crate::supervisor::log::log;
use crate::{clock, procinfo, sys};

/// The model's memory limit: `RLIMIT_AS` on Linux, resident size on macOS.
pub const MEMORY_LIMIT: u64 = 1 << 30;
/// Frames waiting for the model past this mean it has fallen behind.
pub const BACKLOG_LIMIT: usize = 16 << 20;
/// How long the model has to answer a snapshot request.
pub const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(2);
/// How often the supervisor reads the model's resident size on macOS.
pub const MEMORY_CHECK: Duration = Duration::from_secs(10);
/// Restarts allowed within [`RESTART_WINDOW`].
pub const RESTART_LIMIT: usize = 3;
/// See [`RESTART_LIMIT`].
pub const RESTART_WINDOW: Duration = Duration::from_secs(3600);
/// History lines in `tail.txt`.
pub const TAIL_HISTORY: u32 = 1000;

/// The memory limit in effect. Test-hooks builds take
/// `PERCH_TEST_MODEL_MEMORY_LIMIT`, so a test can pass it cheaply.
pub fn memory_limit() -> u64 {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = std::env::var("PERCH_TEST_MODEL_MEMORY_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return n;
    }
    MEMORY_LIMIT
}

/// What the supervisor acts on after talking to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The model failed and was killed. Queries waiting on it go
    /// unanswered.
    Failed,
    /// The DSR 6 reply to the query ending frame `frame`.
    Reply { frame: u64, bytes: Vec<u8> },
    /// A snapshot the supervisor asked for.
    Snapshot { id: u64, tail: Vec<u8> },
}

/// One live model process.
struct Link {
    pid: i32,
    sock: UnixStream,
    /// Encoded messages not yet written, from `out_off`.
    out: Vec<u8>,
    out_off: usize,
    /// Bytes written to the socket so far.
    written: u64,
    /// Where the bytes released at once for a sequence over 16 MiB end,
    /// counted like `written`. The model is not behind for those.
    burst_end: u64,
    decoder: Decoder,
    /// The snapshot asked for and its deadline.
    snapshot: Option<(u64, Instant)>,
    next_memory_check: Instant,
}

impl Link {
    fn backlog(&self) -> usize {
        self.out.len() - self.out_off
    }

    /// The backlog, less what is left of a released long sequence.
    fn behind(&self) -> usize {
        let burst =
            usize::try_from(self.burst_end.saturating_sub(self.written)).unwrap_or(usize::MAX);
        self.backlog().saturating_sub(burst)
    }
}

/// The supervisor's handle on the model.
pub struct Model {
    cols: u16,
    rows: u16,
    scrollback: u32,
    generation: u32,
    link: Option<Link>,
    state: ModelState,
    restarts: VecDeque<Instant>,
    /// Whether the restart limit was logged since the last failure.
    limit_logged: bool,
    pwd: Option<Vec<u8>>,
    next_snapshot: u64,
    events: Vec<Event>,
}

impl Model {
    /// A handle with no model process yet.
    pub fn new(cols: u16, rows: u16, scrollback: u32) -> Model {
        Model {
            cols,
            rows,
            scrollback,
            generation: 0,
            link: None,
            state: ModelState::Failed,
            restarts: VecDeque::new(),
            limit_logged: false,
            pwd: None,
            next_snapshot: 1,
            events: Vec::new(),
        }
    }

    /// Forks a model process. A first model is complete; a restarted one
    /// is partial.
    pub fn start(&mut self, partial: bool) {
        self.generation += 1;
        let params = process::Params {
            generation: self.generation,
            cols: self.cols,
            rows: self.rows,
            scrollback: self.scrollback,
            memory_limit: memory_limit(),
        };
        match spawn(params) {
            Ok((pid, sock)) => {
                log(&format!(
                    "model process {pid} started (generation {}{})",
                    self.generation,
                    if partial { ", partial" } else { "" }
                ));
                self.link = Some(Link {
                    pid,
                    sock,
                    out: Vec::new(),
                    out_off: 0,
                    written: 0,
                    burst_end: 0,
                    decoder: Decoder::new(),
                    snapshot: None,
                    next_memory_check: Instant::now() + clock::scaled(MEMORY_CHECK),
                });
                self.state = if partial {
                    ModelState::Partial
                } else {
                    ModelState::Ok
                };
                self.limit_logged = false;
            }
            Err(e) => {
                log(&format!("cannot fork the model process: {e}"));
                self.state = ModelState::Failed;
            }
        }
    }

    pub fn state(&self) -> ModelState {
        self.state
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn pid(&self) -> Option<i32> {
        self.link.as_ref().map(|l| l.pid)
    }

    /// The URL of the last OSC 7 the program wrote, as a model saw it.
    pub fn pwd(&self) -> Option<&[u8]> {
        self.pwd.as_deref()
    }

    /// What happened since the last call, in order.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Queues frame `n` for the model. A model that falls more than
    /// [`BACKLOG_LIMIT`] behind has failed.
    pub fn send_frame(&mut self, n: u64, bytes: &[u8]) {
        let Some(link) = &mut self.link else { return };
        wire::encode_frame(&mut link.out, n, bytes);
    }

    /// The frames just queued are a sequence over 16 MiB, released at once.
    /// A model that is keeping up needs time to get through them, so they
    /// do not count as falling behind.
    pub fn released_long_sequence(&mut self) {
        if let Some(l) = &mut self.link {
            l.burst_end = l.written + l.backlog() as u64;
        }
    }

    /// Fails the model if it is more than [`BACKLOG_LIMIT`] behind. Called
    /// once per read of program output, before its frames are queued.
    pub fn check_backlog(&mut self) {
        if self
            .link
            .as_ref()
            .is_some_and(|l| l.behind() > BACKLOG_LIMIT)
        {
            self.fail("it fell more than 16 MiB behind");
        }
    }

    /// The program reset the terminal with RIS: a partial model is
    /// complete again. Returns whether it was partial.
    pub fn reset_seen(&mut self) -> bool {
        if self.state == ModelState::Partial && self.link.is_some() {
            self.state = ModelState::Ok;
            log(&format!(
                "model (generation {}) is complete again after RIS",
                self.generation
            ));
            return true;
        }
        false
    }

    /// The model cannot know the screen any more. Returns whether it was
    /// complete.
    pub fn mark_partial(&mut self) -> bool {
        if self.state == ModelState::Ok {
            self.state = ModelState::Partial;
            return true;
        }
        false
    }

    /// Asks a complete model for the tail of its screen. `None` if there is
    /// no complete model.
    pub fn request_snapshot(&mut self, history_rows: u32) -> Option<u64> {
        if self.state != ModelState::Ok {
            return None;
        }
        let link = self.link.as_mut()?;
        let id = self.next_snapshot;
        self.next_snapshot += 1;
        ToModel::Snapshot { id, history_rows }.encode(&mut link.out);
        link.snapshot = Some((id, Instant::now() + SNAPSHOT_DEADLINE));
        Some(id)
    }

    /// Kills the model and carries on without it.
    pub fn fail(&mut self, why: &str) {
        self.drop_link(why, true);
    }

    fn drop_link(&mut self, why: &str, kill: bool) {
        let Some(link) = self.link.take() else { return };
        if kill {
            let _ = sys::kill(link.pid, sys::sig::KILL);
        }
        log(&format!(
            "model process {} (generation {}) failed: {why}; passthrough continues",
            link.pid, self.generation
        ));
        self.state = ModelState::Failed;
        self.events.push(Event::Failed);
    }

    /// Starts a fresh, partial model if one is due: the model has failed,
    /// fewer than [`RESTART_LIMIT`] restarts happened in the last
    /// [`RESTART_WINDOW`], and the stream is at `ground`. Returns whether
    /// it started one.
    pub fn restart_if_due(&mut self, ground: bool) -> bool {
        if self.link.is_some() || self.generation == 0 || !ground {
            return false;
        }
        let now = Instant::now();
        let window = clock::scaled(RESTART_WINDOW);
        while self
            .restarts
            .front()
            .is_some_and(|t| now.duration_since(*t) >= window)
        {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= RESTART_LIMIT {
            if !self.limit_logged {
                log(&format!(
                    "model restarted {RESTART_LIMIT} times within an hour; not restarting it for now"
                ));
                self.limit_logged = true;
            }
            return false;
        }
        self.restarts.push_back(now);
        self.start(true);
        self.link.is_some()
    }

    /// The socket and whether there is something to write.
    pub fn poll_fd(&self) -> Option<(RawFd, bool)> {
        self.link
            .as_ref()
            .map(|l| (l.sock.as_raw_fd(), l.backlog() > 0))
    }

    /// Reads whatever the model sent.
    pub fn read(&mut self) {
        let Some(link) = &mut self.link else { return };
        let mut buf = [0u8; 64 * 1024];
        let mut closed = None;
        loop {
            match link.sock.read(&mut buf) {
                Ok(0) => {
                    closed = Some("its socket closed".to_owned());
                    break;
                }
                Ok(n) => link.decoder.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    closed = Some(format!("cannot read from it: {e}"));
                    break;
                }
            }
        }
        loop {
            let Some(link) = &mut self.link else { return };
            match link.decoder.next_from_model() {
                Ok(Some(msg)) => self.receive(msg),
                Ok(None) => break,
                Err(e) => {
                    self.fail(&format!("it sent {e}"));
                    return;
                }
            }
        }
        if let Some(why) = closed {
            self.fail(&why);
        }
    }

    fn receive(&mut self, msg: FromModel) {
        if msg.generation() != self.generation {
            log(&format!(
                "discarded a message from model generation {} (current {})",
                msg.generation(),
                self.generation
            ));
            return;
        }
        match msg {
            FromModel::Reply {
                frame, row, col, ..
            } => self.events.push(Event::Reply {
                frame,
                bytes: format!("\x1b[{row};{col}R").into_bytes(),
            }),
            FromModel::Pwd { url, .. } => self.pwd = Some(url),
            FromModel::Snapshot {
                id,
                at_ground,
                tail,
                ..
            } => {
                let link = self.link.as_mut().expect("a live model sent it");
                if link.snapshot.is_some_and(|(want, _)| want == id) {
                    link.snapshot = None;
                }
                if !at_ground {
                    log("the model was not at ground at a snapshot, where the tracker was");
                }
                self.events.push(Event::Snapshot { id, tail });
            }
        }
    }

    /// Writes queued messages without blocking.
    pub fn flush(&mut self) {
        let Some(link) = &mut self.link else { return };
        while link.backlog() > 0 {
            match link.sock.write(&link.out[link.out_off..]) {
                Ok(0) => break,
                Ok(n) => {
                    link.out_off += n;
                    link.written += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let why = format!("cannot write to it: {e}");
                    self.fail(&why);
                    return;
                }
            }
        }
        if link.out_off == link.out.len() {
            link.out.clear();
            link.out_off = 0;
        } else if link.out_off > 1 << 20 && link.out_off * 2 > link.out.len() {
            link.out.drain(..link.out_off);
            link.out_off = 0;
        }
    }

    /// A child of the supervisor other than the program ended: a model
    /// process, current or old.
    pub fn reaped(&mut self, pid: i32, status: sys::WaitStatus) {
        let how = Exit::from_status(status).describe();
        if self.link.as_ref().is_some_and(|l| l.pid == pid) {
            // Reaped, so its PID may already be someone else's: no kill.
            self.drop_link(&format!("it ended: {how}"), false);
        } else {
            // An old model, already failed and killed.
            log(&format!("model process {pid} ended: {how}"));
        }
    }

    /// Enforces the snapshot deadline and, on macOS, the memory limit.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let Some(link) = &mut self.link else { return };
        if link.snapshot.is_some_and(|(_, deadline)| now >= deadline) {
            self.fail("it missed its 2-second snapshot deadline");
            return;
        }
        if cfg!(target_os = "macos") && now >= link.next_memory_check {
            link.next_memory_check = now + clock::scaled(MEMORY_CHECK);
            let limit = memory_limit();
            if let Some(rss) = procinfo::resident_bytes(link.pid)
                && rss > limit
            {
                self.fail(&format!(
                    "its resident memory, {rss} bytes, is over the limit of {limit}"
                ));
            }
        }
    }

    /// When [`Model::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        let link = self.link.as_ref()?;
        let memory = cfg!(target_os = "macos").then_some(link.next_memory_check);
        link.snapshot
            .map(|(_, t)| t)
            .into_iter()
            .chain(memory)
            .min()
    }

    /// Test hooks: stage a fault in the model, or kill it.
    #[cfg(feature = "test-hooks")]
    pub fn inject(&mut self, fault: &str) -> Result<(), String> {
        let link = self.link.as_mut().ok_or("no model process is running")?;
        if fault == "kill" {
            let _ = sys::kill(link.pid, sys::sig::KILL);
            return Ok(());
        }
        let f = wire::Fault::parse(fault).ok_or_else(|| format!("unknown fault `{fault}`"))?;
        ToModel::Fault(f).encode(&mut link.out);
        Ok(())
    }
}

/// Forks a model process on one end of a new socket pair, and returns its
/// PID and the other end, non-blocking.
fn spawn(params: process::Params) -> io::Result<(i32, UnixStream)> {
    // Both ends are close-on-exec; the program never sees them.
    let (ours, theirs) = UnixStream::pair()?;
    // SAFETY: the supervisor is single-threaded.
    match unsafe { sys::fork() }? {
        sys::Fork::Child => process::main(OwnedFd::from(theirs), params),
        sys::Fork::Parent(pid) => {
            drop(theirs);
            ours.set_nonblocking(true)?;
            Ok((pid, ours))
        }
    }
}

/// The path in an OSC 7 URL (`file://host/path`, percent-encoded), or
/// `None` if it is not one.
pub fn osc7_path(url: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(url).ok()?;
    let rest = text
        .strip_prefix("file://")
        .or_else(|| text.strip_prefix("kitty-shell-cwd://"))?;
    let path = &rest[rest.find('/')?..];
    let mut out = Vec::with_capacity(path.len());
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_urls() {
        assert_eq!(
            osc7_path(b"file://host/home/me/a%20b").as_deref(),
            Some("/home/me/a b")
        );
        assert_eq!(osc7_path(b"file:///tmp").as_deref(), Some("/tmp"));
        assert_eq!(
            osc7_path(b"kitty-shell-cwd://h/x%2").as_deref(),
            Some("/x%2")
        );
        assert_eq!(osc7_path(b"http://h/x"), None);
        assert_eq!(osc7_path(b"file://host"), None);
    }
}
