// SPDX-License-Identifier: GPL-3.0-only

//! The supervisor: one process per session, started by `perch new` with a
//! double fork, owning the PTY master, the session directory and its lock,
//! and the control socket. It runs one single-threaded `poll` loop.
//!
//! It reads the program's output into the boundary tracker and the model
//! process (see [`crate::model`]), answers the program's terminal queries
//! while no client is attached (DSR 5, DA1 and DA2 itself, DSR 6 from the
//! model), answers control requests, keeps its directory published, and
//! ends the session, leaving `tail.txt` when the program exits detached.
//! Attaching comes later.

pub(crate) mod log;
mod program;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use self::log::log;
pub use self::log::{CAP as LOG_CAP, FILE as LOG_FILE};
pub use self::program::StartError;
use crate::input::Input;
use crate::meta::{self, Exit, Meta};
use crate::model::feed::{Feeder, Step};
use crate::model::{self as model_mod, Model};
use crate::proto::{self, Decoder, ErrorBody, Hello, Welcome, kind};
use crate::queries::{self, ModelState, Queries, QueryId};
use crate::runtime::{self, RuntimeDir};
use crate::sys::{self, Lock, PollFd};
use crate::transport::Transport;
use crate::{clock, procinfo};

/// How often the supervisor checks that its directory and socket are still
/// its own.
pub const IDENTITY_CHECK: Duration = Duration::from_secs(60);
/// How often it updates the timestamps of everything in its directory.
pub const TOUCH: Duration = Duration::from_secs(3600);
/// How long a connection may take to send `Hello`.
pub const HELLO_DEADLINE: Duration = Duration::from_secs(5);
/// Between SIGHUP and SIGKILL when killing.
pub const KILL_GRACE: Duration = Duration::from_secs(5);
/// How often the kill scope is rescanned while a kill is in progress.
const KILL_SCAN: Duration = Duration::from_millis(50);
/// After SIGKILL, how long to wait for the scope to empty before giving up.
const KILL_FINAL: Duration = Duration::from_secs(2);
/// A connection whose unsent responses exceed this is dropped.
const MAX_CONN_BACKLOG: usize = 4 << 20;

/// Operations this supervisor supports, announced in `Welcome`.
pub const CAPABILITIES: &[&str] = &["info", "kill", "rename", "annotate", "detach"];

/// The degraded marker for a recreated directory.
pub const DIRECTORY_RECREATED: &str = "directory-recreated";

/// Everything `perch new` decides before the supervisor starts.
pub struct Config {
    pub runtime: PathBuf,
    pub id: String,
    pub name: String,
    pub kind: String,
    pub owner: Option<String>,
    pub annotations: BTreeMap<String, String>,
    /// The command, placeholders already expanded.
    pub command: Vec<String>,
    /// The program's whole environment as `NAME=VALUE`.
    pub env: Vec<OsString>,
    pub cwd: PathBuf,
    pub umask: u32,
    pub size: sys::Winsize,
    /// The creator's terminal attributes, if it had a terminal.
    pub termios: Option<sys::Termios>,
    pub scrollback: u32,
}

/// What `perch new -d` learns from the supervisor.
#[derive(Debug)]
pub enum Outcome {
    /// The program started.
    Running { pid: i32 },
    /// The program could not start; the session is gone.
    StartFailed { message: String, exit_code: u8 },
}

/// Starts a supervisor for `cfg` and waits until its socket is listening
/// and the program has started or failed to.
pub fn start(cfg: Config) -> Result<Outcome, String> {
    let (ready_r, ready_w) = sys::pipe().map_err(|e| format!("pipe: {e}"))?;
    // SAFETY: the CLI is single-threaded here.
    match unsafe { sys::fork() }.map_err(|e| format!("fork: {e}"))? {
        sys::Fork::Child => {
            drop(ready_r);
            // A new session, then a second fork so the supervisor is not a
            // session leader and cannot reacquire a controlling terminal.
            if sys::setsid().is_err() {
                sys::exit_now(1);
            }
            // SAFETY: still single-threaded.
            match unsafe { sys::fork() } {
                Ok(sys::Fork::Child) => sys::exit_now(run(cfg, ready_w)),
                Ok(sys::Fork::Parent(_)) => sys::exit_now(0),
                Err(_) => sys::exit_now(1),
            }
        }
        sys::Fork::Parent(pid) => {
            drop(ready_w);
            let _ = sys::waitpid(pid, false);
            read_readiness(ready_r)
        }
    }
}

fn read_readiness(fd: OwnedFd) -> Result<Outcome, String> {
    let mut text = String::new();
    let _ = std::fs::File::from(fd).read_to_string(&mut text);
    let mut ready = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["event"].as_str() {
            Some("ready") => ready = true,
            Some("running") => {
                return Ok(Outcome::Running {
                    pid: v["pid"].as_i64().unwrap_or(0) as i32,
                });
            }
            Some("start_failed") => {
                return Ok(Outcome::StartFailed {
                    message: v["message"]
                        .as_str()
                        .unwrap_or("the program did not start")
                        .into(),
                    exit_code: v["exitCode"]
                        .as_u64()
                        .and_then(|c| u8::try_from(c).ok())
                        .unwrap_or(1),
                });
            }
            Some("error") => return Err(v["message"].as_str().unwrap_or("unknown error").into()),
            _ => {}
        }
    }
    Err(if ready {
        "the supervisor stopped before the program started; see supervisor.log".into()
    } else {
        "the supervisor failed to start".into()
    })
}

/// The write end of the readiness pipe.
struct Ready(Option<OwnedFd>);

impl Ready {
    /// Sends one event. Returns whether the creator could receive it.
    fn send(&mut self, event: &Value) -> bool {
        let Some(fd) = &self.0 else { return false };
        let mut line = event.to_string();
        line.push('\n');
        sys::write_all(fd.as_raw_fd(), line.as_bytes()).is_ok()
    }

    fn close(&mut self) {
        self.0 = None;
    }
}

/// The grandchild: becomes the supervisor. Returns the exit code.
fn run(cfg: Config, ready_w: OwnedFd) -> i32 {
    sys::set_umask(0o077);
    let _ = sys::ignore_signal(sys::sig::HUP);
    let _ = sys::ignore_signal(sys::sig::PIPE);
    let _ = sys::chdir(std::path::Path::new("/"));
    // Drop everything inherited from the creator except the pipe.
    sys::close_fds_from(3, &[ready_w.as_raw_fd()]);
    if let Ok(null) = sys::open_rw_noctty(std::path::Path::new("/dev/null")) {
        for fd in 0..3 {
            let _ = sys::dup2(null.as_raw_fd(), fd);
        }
        if null.as_raw_fd() <= 2 {
            std::mem::forget(null);
        }
    }
    let mut ready = Ready(Some(ready_w));
    match Supervisor::setup(cfg) {
        Ok(mut sup) => {
            ready.send(&json!({ "event": "ready", "id": sup.meta.id }));
            sup.run(ready)
        }
        Err(msg) => {
            ready.send(&json!({ "event": "error", "message": msg }));
            1
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    AwaitHello,
    Control,
}

struct Conn {
    stream: UnixStream,
    decoder: Decoder,
    out: Vec<u8>,
    phase: Phase,
    hello_deadline: Instant,
    /// Close once `out` is flushed.
    closing: bool,
    /// The peer has closed its side; answer what it sent, then close.
    eof: bool,
    dead: bool,
}

impl Conn {
    fn finished(&self) -> bool {
        self.dead || ((self.closing || self.eof) && self.out.is_empty())
    }
}

/// Why a kill was started: an op, whose requester knows, or SIGTERM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillOrigin {
    Request,
    Signal,
}

struct Killing {
    origin: KillOrigin,
    deadline: Instant,
    sigkill_sent: bool,
    give_up: Option<Instant>,
    next_scan: Instant,
}

struct Supervisor {
    cfg_runtime: PathBuf,
    rt: RuntimeDir,
    dir: OwnedFd,
    listener: UnixListener,
    sock_stat: sys::Stat,
    meta: Meta,
    master: Option<OwnedFd>,
    master_readable: bool,
    slave: Option<OwnedFd>,
    size: sys::Winsize,
    plan: Option<program::Plan>,
    program: Option<i32>,
    program_status: Option<sys::WaitStatus>,
    start_pipe: Option<OwnedFd>,
    start_buf: Vec<u8>,
    sig_r: OwnedFd,
    _sig_w: OwnedFd,
    conns: Vec<Conn>,
    last_output: Option<i64>,
    next_identity: Instant,
    next_touch: Instant,
    killing: Option<Killing>,
    conflict_logged: bool,
    cwd: PathBuf,
    /// The output side: tracker and what the model is fed.
    feeder: Feeder,
    model: Model,
    queries: Queries,
    input: Input,
    /// Writing to the master failed for good.
    master_write_failed: bool,
    /// The origin of the time `queries` and `input` count in.
    epoch: Instant,
    /// The session is ending with a tombstone, waiting for the tail.
    ending: Option<Ending>,
}

/// A tombstone waiting for the model's snapshot of the screen.
struct Ending {
    /// The snapshot asked for, or `None` when the model is not complete.
    snapshot: Option<u64>,
    tail: Option<Vec<u8>>,
}

/// What `tail.txt` gets.
enum Tail {
    Screen(Vec<u8>),
    Note(ModelState),
}

/// Writes to the PTY master without blocking.
struct Master<'a> {
    fd: i32,
    failed: &'a mut bool,
}

impl Transport for Master<'_> {
    fn write(&mut self, bytes: &[u8]) -> usize {
        match sys::write(self.fd, bytes) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
            Err(e) => {
                log(&format!("cannot write to the PTY: {e}"));
                *self.failed = true;
                0
            }
        }
    }
}

/// Where the supervisor binds its socket: inside the directory being built.
fn bind_socket(rt: &RuntimeDir, dir_name: &OsStr, sock_name: &str) -> io::Result<UnixListener> {
    let path = rt.path.join(dir_name).join(sock_name);
    // 0600: the umask decides a socket's mode.
    let old = sys::set_umask(0o177);
    let res = UnixListener::bind(&path);
    sys::set_umask(old);
    let listener = res?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Builds a directory `.new-<random>` with the lock, the log and a socket.
fn build_dir(rt: &RuntimeDir) -> io::Result<(OsString, OwnedFd, UnixListener, sys::Stat)> {
    let name = OsString::from(format!(".new-{}", runtime::random_name(8)));
    sys::mkdir_at(rt.fd.as_fd(), &name, 0o700)?;
    let built = (|| {
        let dir = sys::open_dir_at(rt.fd.as_fd(), &name)?;
        if !sys::try_flock(dir.as_fd(), Lock::Exclusive)? {
            return Err(io::Error::other("cannot lock the new session directory"));
        }
        let logf = sys::open_file_at(dir.as_fd(), OsStr::new(LOG_FILE), sys::OpenMode::Append)?;
        sys::dup2(logf.as_raw_fd(), 2)?;
        drop(logf);
        let listener = bind_socket(rt, &name, "sock")?;
        let sock_stat = sys::fstatat(dir.as_fd(), OsStr::new("sock"))?;
        Ok((dir, listener, sock_stat))
    })();
    match built {
        Ok((dir, l, s)) => Ok((name, dir, l, s)),
        Err(e) => {
            if let Ok(fd) = sys::open_dir_at(rt.fd.as_fd(), &name) {
                let _ = runtime::remove_locked(rt.fd.as_fd(), &name, fd.as_fd());
            }
            Err(e)
        }
    }
}

impl Supervisor {
    fn setup(cfg: Config) -> Result<Supervisor, String> {
        let rt = runtime::open(&cfg.runtime)?;
        let (build, dir, listener, sock_stat) =
            build_dir(&rt).map_err(|e| format!("cannot create the session directory: {e}"))?;
        log(&format!("supervisor for session {} starting", cfg.id));

        let fail = |msg: String| {
            log(&msg);
            let _ = runtime::remove_locked(rt.fd.as_fd(), &build, dir.as_fd());
            msg
        };

        // The PTY, with the creator's size and termios or defaults.
        let (master, slave_path) =
            sys::open_pty().map_err(|e| fail(format!("cannot open a PTY: {e}")))?;
        let slave = sys::open_rw_noctty(&slave_path)
            .map_err(|e| fail(format!("cannot open {}: {e}", slave_path.display())))?;
        let termios = match cfg.termios {
            Some(t) => t,
            None => {
                let mut t = sys::tcgetattr(slave.as_raw_fd())
                    .ok_or_else(|| fail("tcgetattr failed".into()))?;
                t.set_iutf8(true);
                t
            }
        };
        sys::tcsetattr(slave.as_raw_fd(), &termios).map_err(|e| fail(format!("tcsetattr: {e}")))?;
        sys::set_winsize(master.as_raw_fd(), cfg.size)
            .map_err(|e| fail(format!("TIOCSWINSZ: {e}")))?;
        sys::set_nonblocking(master.as_raw_fd(), true).map_err(|e| fail(e.to_string()))?;

        let plan = program::Plan::new(&cfg.command, &cfg.env, &cfg.cwd, cfg.umask)
            .map_err(|e| fail(format!("cannot prepare the command: {e}")))?;

        let (sig_r, sig_w) = sys::pipe().map_err(|e| fail(e.to_string()))?;
        sys::set_nonblocking(sig_r.as_raw_fd(), true).map_err(|e| fail(e.to_string()))?;
        sys::set_nonblocking(sig_w.as_raw_fd(), true).map_err(|e| fail(e.to_string()))?;
        sys::route_signals_to_pipe(&[sys::sig::CHLD, sys::sig::TERM], sig_w.as_raw_fd())
            .map_err(|e| fail(e.to_string()))?;

        let me = sys::getpid();
        let meta = Meta {
            format: meta::FORMAT,
            id: cfg.id.clone(),
            name: cfg.name,
            kind: cfg.kind,
            owner: cfg.owner,
            annotations: cfg.annotations,
            command: cfg.command,
            cwd: cfg.cwd.to_string_lossy().into_owned(),
            host: sys::hostname(),
            created_at: clock::rfc3339(clock::unix_now()),
            supervisor_pid: me,
            supervisor_start: procinfo::start_time(me).unwrap_or_default(),
            pid: None,
            pid_start: None,
            perch_version: crate::VERSION.into(),
            protocol: proto::PROTOCOL,
            state: meta::STARTING.into(),
            exit: None,
            degraded: Vec::new(),
            scrollback: cfg.scrollback,
            ended_at: None,
            extra: serde_json::Map::new(),
        };
        meta::write(dir.as_fd(), &meta)
            .map_err(|e| fail(format!("cannot write meta.json: {e}")))?;

        // Publish: one atomic rename. A name already taken fails.
        if let Err(e) = sys::rename_noreplace_at(rt.fd.as_fd(), &build, OsStr::new(&cfg.id)) {
            let msg = match e.raw_os_error() {
                Some(sys::errno::EEXIST | sys::errno::ENOTEMPTY) => {
                    format!("a session with ID {} already exists", cfg.id)
                }
                _ => format!("cannot publish the session directory: {e}"),
            };
            return Err(fail(msg));
        }
        log(&format!("published {}", rt.path.join(&cfg.id).display()));

        let now = Instant::now();
        Ok(Supervisor {
            cfg_runtime: cfg.runtime,
            rt,
            dir,
            listener,
            sock_stat,
            meta,
            master: Some(master),
            master_readable: true,
            slave: Some(slave),
            size: cfg.size,
            plan: Some(plan),
            program: None,
            program_status: None,
            start_pipe: None,
            start_buf: Vec::new(),
            sig_r,
            _sig_w: sig_w,
            conns: Vec::new(),
            last_output: None,
            next_identity: now + clock::scaled(IDENTITY_CHECK),
            next_touch: now + clock::scaled(TOUCH),
            killing: None,
            conflict_logged: false,
            cwd: cfg.cwd,
            feeder: Feeder::new(),
            model: Model::new(cfg.size.cols, cfg.size.rows, cfg.scrollback),
            queries: Queries::new(),
            input: Input::new(),
            master_write_failed: false,
            epoch: now,
            ending: None,
        })
    }

    fn write_meta(&self) {
        if let Err(e) = meta::write(self.dir.as_fd(), &self.meta) {
            log(&format!("cannot write meta.json: {e}"));
        }
    }

    fn run(&mut self, mut ready: Ready) -> i32 {
        // The program is forked after readiness (`-d`).
        let plan = self.plan.take().expect("plan");
        let slave = self.slave.take().expect("slave");
        match program::spawn(&plan, &slave) {
            Ok((pid, pipe)) => {
                let _ = sys::set_nonblocking(pipe.as_raw_fd(), true);
                self.program = Some(pid);
                self.meta.pid = Some(pid);
                self.meta.pid_start = procinfo::start_time(pid);
                self.start_pipe = Some(pipe);
            }
            Err(e) => {
                let msg = format!("cannot fork the program: {e}");
                log(&msg);
                let delivered =
                    ready.send(&json!({ "event": "start_failed", "message": msg, "exitCode": 1 }));
                return self.end_start_failed(msg, delivered);
            }
        }
        drop(slave);
        drop(plan);
        self.model.start(false);
        self.apply_model_state();

        loop {
            let mut fds = vec![PollFd::new(self.sig_r.as_raw_fd(), true, false)];
            fds.push(PollFd::new(self.listener.as_raw_fd(), true, false));
            let master_write = self.input.pending().is_some() && !self.master_write_failed;
            let master_idx = match &self.master {
                Some(m) if self.master_readable || master_write => {
                    fds.push(PollFd::new(
                        m.as_raw_fd(),
                        self.master_readable,
                        master_write,
                    ));
                    Some(fds.len() - 1)
                }
                _ => None,
            };
            let model_idx = self.model.poll_fd().map(|(fd, write)| {
                fds.push(PollFd::new(fd, true, write));
                fds.len() - 1
            });
            let start_idx = self.start_pipe.as_ref().map(|p| {
                fds.push(PollFd::new(p.as_raw_fd(), true, false));
                fds.len() - 1
            });
            let conn_base = fds.len();
            for c in &self.conns {
                fds.push(PollFd::new(c.stream.as_raw_fd(), !c.eof, !c.out.is_empty()));
            }
            let timeout = self.next_wake().map(|t| {
                let d = t.saturating_duration_since(Instant::now());
                d.as_millis().min(u128::from(u32::MAX)) as u64
                    + u64::from(d.subsec_nanos() % 1_000_000 != 0)
            });
            if let Err(e) = sys::poll(&mut fds, timeout) {
                log(&format!("poll: {e}"));
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            if fds[0].readable {
                self.handle_signals();
            }
            if let Some(i) = start_idx
                && (fds[i].readable || fds[i].hangup)
                && let Some(code) = self.handle_start_pipe(&mut ready)
            {
                return code;
            }
            if let Some(i) = master_idx
                && self.master_readable
                && (fds[i].readable || fds[i].hangup)
            {
                self.read_master();
            }
            if let Some(i) = model_idx
                && (fds[i].readable || fds[i].hangup)
            {
                self.model.read();
            }
            self.model.tick();
            let now = self.elapsed();
            let events = self.queries.tick(now);
            self.apply_queries(events);
            self.input.tick(now);
            self.handle_model_events();
            self.model.flush();
            self.handle_model_events();
            self.flush_input();
            for (k, c) in self.conns.iter_mut().enumerate() {
                let p = fds[conn_base + k];
                if p.readable || p.hangup {
                    read_conn(c);
                }
            }
            self.process_conns();
            for (k, c) in self.conns.iter_mut().enumerate() {
                if fds.get(conn_base + k).is_some_and(|p| p.writable) || !c.out.is_empty() {
                    flush_conn(c);
                }
            }
            if fds[1].readable {
                self.accept();
            }
            self.conns.retain(|c| !c.finished());

            let now = Instant::now();
            for c in &mut self.conns {
                if c.phase == Phase::AwaitHello && now >= c.hello_deadline {
                    c.dead = true;
                }
            }
            self.conns.retain(|c| !c.dead);
            if now >= self.next_identity {
                self.next_identity = now + clock::scaled(IDENTITY_CHECK);
                self.check_identity();
            }
            if now >= self.next_touch {
                self.next_touch = now + clock::scaled(TOUCH);
                self.touch_all();
            }
            if let Some(code) = self.progress() {
                self.flush_before_exit();
                return code;
            }
        }
    }

    /// Gives every connection's pending responses (a kill's `ok`) a short
    /// chance to go out before the supervisor exits.
    fn flush_before_exit(&mut self) {
        for c in &mut self.conns {
            if !c.out.is_empty() && !c.dead {
                let _ = c.stream.set_nonblocking(false);
                let _ = c.stream.set_write_timeout(Some(Duration::from_millis(200)));
                let _ = c.stream.write_all(&c.out);
            }
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let mut t = self.next_identity.min(self.next_touch);
        for d in [self.queries.next_deadline(), self.input.next_deadline()]
            .into_iter()
            .flatten()
        {
            t = t.min(self.epoch + d);
        }
        if let Some(d) = self.model.next_deadline() {
            t = t.min(d);
        }
        for c in &self.conns {
            if c.phase == Phase::AwaitHello {
                t = t.min(c.hello_deadline);
            }
        }
        if let Some(k) = &self.killing {
            t = t.min(k.next_scan);
        }
        Some(t)
    }

    fn handle_signals(&mut self) {
        let mut buf = [0u8; 64];
        let mut term = false;
        while let Ok(n) = sys::read(self.sig_r.as_raw_fd(), &mut buf) {
            if n == 0 {
                break;
            }
            term |= buf[..n].contains(&(sys::sig::TERM as u8));
        }
        self.reap();
        if term && self.killing.is_none() {
            log("SIGTERM: killing the session");
            self.start_kill(false, KillOrigin::Signal);
        }
    }

    fn reap(&mut self) {
        while let Ok(Some((pid, status))) = sys::waitpid(-1, true) {
            if Some(pid) == self.program {
                log(&format!(
                    "program {pid} ended: {}",
                    Exit::from_status(status).describe()
                ));
                self.program_status = Some(status);
            } else {
                self.model.reaped(pid, status);
            }
        }
    }

    /// Reads the exec-result pipe. Returns an exit code if the session
    /// ended because the program could not start.
    fn handle_start_pipe(&mut self, ready: &mut Ready) -> Option<i32> {
        let fd = self.start_pipe.as_ref()?.as_raw_fd();
        let mut buf = [0u8; 16];
        loop {
            match sys::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => self.start_buf.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                Err(_) => break,
            }
        }
        self.start_pipe = None;
        match StartError::decode(&self.start_buf) {
            None => {
                self.meta.state = meta::RUNNING.into();
                self.write_meta();
                log(&format!("program {} running", self.program.unwrap_or(0)));
                ready.send(&json!({ "event": "running", "pid": self.program }));
                ready.close();
                None
            }
            Some(err) => {
                let msg = err.describe(
                    self.meta.command.first().map_or("", String::as_str),
                    &self.cwd,
                );
                log(&msg);
                // The child exits at once; collect it.
                if let Some(pid) = self.program {
                    let _ = sys::waitpid(pid, false);
                }
                let delivered = ready.send(&json!({
                    "event": "start_failed",
                    "message": msg,
                    "exitCode": err.exit_code(),
                }));
                // The pipe closes only after cleanup, so `perch new`
                // returns once nothing is left behind.
                let code = self.end_start_failed(msg, delivered);
                ready.close();
                Some(code)
            }
        }
    }

    /// The program never started. If the creator heard why, nothing is
    /// left behind; otherwise a tombstone keeps the error.
    fn end_start_failed(&mut self, msg: String, delivered: bool) -> i32 {
        if delivered {
            self.remove_dir();
        } else {
            self.meta.exit = Some(Exit {
                error: Some(msg),
                ..Exit::default()
            });
            self.tombstone(Tail::Screen(Vec::new()));
        }
        1
    }

    /// Reads the program's output, up to `rounds` reads.
    fn read_master_rounds(&mut self, rounds: usize) {
        let Some(m) = &self.master else { return };
        let fd = m.as_raw_fd();
        let mut buf = vec![0u8; 64 * 1024];
        for _ in 0..rounds {
            match sys::read(fd, &mut buf) {
                Ok(0) => {
                    self.master_readable = false;
                    return;
                }
                Ok(n) => {
                    self.last_output = Some(clock::unix_now());
                    self.output(&buf[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                // EIO: every slave descriptor is closed.
                Err(_) => {
                    self.master_readable = false;
                    return;
                }
            }
        }
    }

    fn read_master(&mut self) {
        self.read_master_rounds(16);
    }

    /// Output read from the PTY: through the tracker to the model, with
    /// each query given its owner.
    fn output(&mut self, chunk: &[u8]) {
        if self.model.restart_if_due(self.feeder.at_ground_point()) {
            self.apply_model_state();
        }
        for step in self.feeder.push(chunk) {
            match step {
                Step::Frame { n, bytes } => self.model.send_frame(n, &bytes),
                Step::Query { id, kind } => {
                    // The model may have failed on this read already.
                    self.handle_model_events();
                    let events = self.queries.query(id, kind, self.elapsed());
                    self.apply_queries(events);
                }
                Step::Reset => {
                    if self.model.reset_seen() {
                        self.apply_model_state();
                    }
                }
                Step::Discarded => {
                    log("a sequence over 16 MiB was not given to the model");
                }
                Step::Partial => {
                    log("a sequence over 16 MiB was fed to the model in pieces; it is partial");
                    if self.model.mark_partial() {
                        self.apply_model_state();
                    }
                }
            }
        }
        if self.model.restart_if_due(self.feeder.at_ground_point()) {
            self.apply_model_state();
        }
        self.handle_model_events();
    }

    fn elapsed(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// Tells `queries` the model's state.
    fn apply_model_state(&mut self) {
        let events = self.queries.set_model(self.model.state());
        self.apply_queries(events);
    }

    fn apply_queries(&mut self, events: Vec<queries::Event>) {
        for e in events {
            match e {
                queries::Event::Answer { bytes, .. } => self.input.push_reply(bytes),
                queries::Event::Unanswered { .. } | queries::Event::Expired { .. } => {}
                queries::Event::ModelMissedDeadlines => {
                    self.model
                        .fail("it missed 3 DSR 6 deadlines within a minute");
                }
            }
        }
    }

    /// Acts on what the model said or did, until nothing is left.
    fn handle_model_events(&mut self) {
        loop {
            let events = self.model.take_events();
            if events.is_empty() {
                return;
            }
            for e in events {
                match e {
                    model_mod::Event::Failed => self.apply_model_state(),
                    model_mod::Event::Reply { frame, bytes } => {
                        let events =
                            self.queries
                                .model_reply(QueryId(frame), &bytes, self.elapsed());
                        self.apply_queries(events);
                    }
                    model_mod::Event::Snapshot { id, tail } => {
                        if let Some(e) = &mut self.ending
                            && e.snapshot == Some(id)
                        {
                            e.tail = Some(tail);
                        }
                    }
                }
            }
        }
    }

    /// Writes queued replies to the program without blocking.
    fn flush_input(&mut self) {
        let Some(m) = &self.master else { return };
        if self.master_write_failed {
            return;
        }
        let mut t = Master {
            fd: m.as_raw_fd(),
            failed: &mut self.master_write_failed,
        };
        self.input.flush(&mut t);
    }

    fn accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    match sys::peer_uid(stream.as_fd()) {
                        Ok(uid) if runtime::peer_allowed(uid, sys::getuid()) => {}
                        Ok(uid) => {
                            log(&format!("refused a connection from uid {uid}"));
                            continue;
                        }
                        Err(e) => {
                            log(&format!("cannot check a peer's uid: {e}"));
                            continue;
                        }
                    }
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    self.conns.push(Conn {
                        stream,
                        decoder: Decoder::new(),
                        out: Vec::new(),
                        phase: Phase::AwaitHello,
                        hello_deadline: Instant::now() + clock::scaled(HELLO_DEADLINE),
                        closing: false,
                        eof: false,
                        dead: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    log(&format!("accept: {e}"));
                    return;
                }
            }
        }
    }

    fn process_conns(&mut self) {
        let mut requests = Vec::new();
        for (i, c) in self.conns.iter_mut().enumerate() {
            while !c.closing && !c.dead {
                match c.decoder.next_frame() {
                    Ok(None) => break,
                    Ok(Some(f)) => match c.phase {
                        Phase::AwaitHello => handle_hello(c, &f),
                        Phase::Control if f.kind == kind::REQUEST => requests.push((i, f.payload)),
                        Phase::Control => send_error(
                            c,
                            ErrorBody::new(
                                "unexpected_frame",
                                format!("frame type {} is not valid in control mode", f.kind),
                            ),
                        ),
                    },
                    Err(e) => send_error(c, ErrorBody::new("bad_frame", e.to_string())),
                }
            }
        }
        for (i, payload) in requests {
            let resp = self.handle_request(&payload);
            let c = &mut self.conns[i];
            // A reply echoes the request's `id`, so a request near the
            // frame cap can make one too large to send.
            let frame = proto::encode(kind::RESPONSE, &serde_json::to_vec(&resp).expect("JSON"))
                .unwrap_or_else(|_| {
                    let err =
                        proto::error_response(&Value::Null, "too_large", "the reply is too large");
                    proto::encode_json(kind::RESPONSE, &err)
                });
            c.out.extend(frame);
            if c.out.len() > MAX_CONN_BACKLOG {
                c.dead = true;
            }
        }
    }

    fn handle_request(&mut self, payload: &[u8]) -> Value {
        let Ok(Value::Object(req)) = serde_json::from_slice::<Value>(payload) else {
            return proto::error_response(
                &Value::Null,
                "bad_request",
                "a request must be a JSON object",
            );
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let Some(op) = req.get("op").and_then(Value::as_str) else {
            return proto::error_response(&id, "bad_request", "missing `op`");
        };
        match op {
            "info" => proto::ok_response(&id, self.info()),
            "kill" => {
                let force = req.get("force").and_then(Value::as_bool).unwrap_or(false);
                log(&format!(
                    "kill requested{}",
                    if force { " (force)" } else { "" }
                ));
                self.start_kill(force, KillOrigin::Request);
                proto::ok_response(&id, json!({}))
            }
            "rename" => match req.get("name").and_then(Value::as_str) {
                Some(name) if !name.is_empty() => {
                    let mut next = self.meta.clone();
                    next.name = name.to_owned();
                    self.replace_meta(&id, next)
                }
                _ => proto::error_response(&id, "bad_request", "`name` must be a non-empty string"),
            },
            "annotate" => self.annotate(&id, &req),
            "detach" => proto::ok_response(&id, json!({ "detached": false })),
            #[cfg(feature = "test-hooks")]
            "test-model" => self.test_model(&id, &req),
            other => proto::error_response(&id, "unsupported", &format!("unknown op `{other}`")),
        }
    }

    fn annotate(&mut self, id: &Value, req: &serde_json::Map<String, Value>) -> Value {
        let mut set = BTreeMap::new();
        match req.get("set") {
            None | Some(Value::Null) => {}
            Some(Value::Object(m)) => {
                for (k, v) in m {
                    match v.as_str() {
                        Some(v) if !k.is_empty() => {
                            set.insert(k.clone(), v.to_owned());
                        }
                        _ => {
                            return proto::error_response(
                                id,
                                "bad_request",
                                "`set` maps non-empty keys to strings",
                            );
                        }
                    }
                }
            }
            Some(_) => return proto::error_response(id, "bad_request", "`set` must be an object"),
        }
        let mut unset = Vec::new();
        match req.get("unset") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                for k in a {
                    match k.as_str() {
                        Some(k) => unset.push(k.to_owned()),
                        None => {
                            return proto::error_response(
                                id,
                                "bad_request",
                                "`unset` lists strings",
                            );
                        }
                    }
                }
            }
            Some(_) => return proto::error_response(id, "bad_request", "`unset` must be an array"),
        }
        let mut next = self.meta.clone();
        for k in unset {
            next.annotations.remove(&k);
        }
        next.annotations.extend(set);
        self.replace_meta(id, next)
    }

    /// Test hooks: the model's process and generation, and faults to
    /// stage in it.
    #[cfg(feature = "test-hooks")]
    fn test_model(&mut self, id: &Value, req: &serde_json::Map<String, Value>) -> Value {
        if let Some(fault) = req.get("fault").and_then(Value::as_str) {
            log(&format!("test hook: model fault `{fault}`"));
            if let Err(e) = self.model.inject(fault) {
                return proto::error_response(id, "bad_request", &e);
            }
            self.model.flush();
        }
        proto::ok_response(
            id,
            json!({
                "pid": self.model.pid(),
                "generation": self.model.generation(),
                "state": model_state_name(self.model.state()),
                "pwd": self.model.pwd().map(|p| String::from_utf8_lossy(p).into_owned()),
            }),
        )
    }

    /// Writes changed metadata, unless it would outgrow what readers take.
    fn replace_meta(&mut self, id: &Value, next: Meta) -> Value {
        if !next.fits() {
            return proto::error_response(id, "too_large", "meta.json would exceed 1 MiB");
        }
        self.meta = next;
        self.write_meta();
        proto::ok_response(id, json!({}))
    }

    /// The live fields.
    fn info(&self) -> Value {
        let scope = self.scope();
        let memory: u64 = scope
            .iter()
            .filter_map(|&p| procinfo::resident_bytes(p))
            .sum();
        let foreground = self
            .master
            .as_ref()
            .and_then(|m| sys::tcgetpgrp(m.as_raw_fd()).ok())
            .filter(|&pgid| pgid > 0)
            .map(|pgid| json!({ "pid": pgid, "name": procinfo::name(pgid) }));
        json!({
            "state": self.meta.state,
            "exit": self.meta.exit,
            "pid": self.meta.pid,
            "attached": false,
            "clientLabel": null,
            "clientPid": null,
            "rows": self.size.rows,
            "cols": self.size.cols,
            "lastOutputAt": self.last_output.map(clock::rfc3339),
            "lastInputAt": null,
            "degraded": self.meta.degraded,
            "model": model_state_name(self.model.state()),
            "memoryBytes": (!scope.is_empty()).then_some(memory),
            "cwd": self.live_cwd(),
            "foreground": foreground,
        })
    }

    /// The program's directory if it can be read, else the last OSC 7,
    /// else the starting directory.
    fn live_cwd(&self) -> String {
        if let Some(p) = self.program.and_then(procinfo::cwd) {
            return p.to_string_lossy().into_owned();
        }
        if let Some(p) = self.model.pwd().and_then(model_mod::osc7_path) {
            return p;
        }
        self.meta.cwd.clone()
    }

    // -----------------------------------------------------------------
    // Ending the session

    fn scope(&self) -> Vec<i32> {
        let Some(pid) = self.program else {
            return Vec::new();
        };
        let mut members = procinfo::session_members(pid);
        if self.program_status.is_none() && !members.contains(&pid) {
            members.push(pid);
        }
        members
    }

    fn start_kill(&mut self, force: bool, origin: KillOrigin) {
        if let Some(k) = &mut self.killing {
            // A second request can only escalate.
            if force && !k.sigkill_sent {
                k.deadline = Instant::now();
            }
            if origin == KillOrigin::Request {
                k.origin = KillOrigin::Request;
            }
            return;
        }
        // Closing the master is what closing a terminal window did.
        self.master = None;
        if !force {
            for pid in self.scope() {
                let _ = sys::kill(pid, sys::sig::HUP);
                let _ = sys::kill(pid, sys::sig::CONT);
            }
        }
        let now = Instant::now();
        self.killing = Some(Killing {
            origin,
            deadline: if force {
                now
            } else {
                now + clock::scaled(KILL_GRACE)
            },
            sigkill_sent: false,
            give_up: None,
            next_scan: now,
        });
    }

    /// Advances a kill or a natural exit. Returns an exit code when the
    /// supervisor should exit.
    fn progress(&mut self) -> Option<i32> {
        if self.ending.is_some() {
            return self.finish_ending();
        }
        if self.start_pipe.is_some() && self.killing.is_none() {
            return None;
        }
        let Some(k) = &self.killing else {
            // Exit while detached: a tombstone, after the output still in
            // the PTY has reached the model.
            let status = self.program_status?;
            self.read_master_rounds(64);
            self.meta.exit = Some(Exit::from_status(status));
            return self.begin_ending();
        };
        let now = Instant::now();
        if now < k.next_scan {
            return None;
        }
        let members = self.scope();
        let k = self.killing.as_mut().expect("killing");
        k.next_scan = now + KILL_SCAN;
        let done = members.is_empty() && self.program_status.is_some();
        let gave_up = k.give_up.is_some_and(|t| now >= t);
        if !done && !gave_up {
            if !k.sigkill_sent && now >= k.deadline {
                for &pid in &members {
                    let _ = sys::kill(pid, sys::sig::KILL);
                }
                k.sigkill_sent = true;
                k.give_up = Some(now + KILL_FINAL);
            }
            return None;
        }
        if gave_up {
            log(&format!(
                "processes still in the session after SIGKILL: {members:?}"
            ));
        }
        let origin = k.origin;
        self.meta.exit = self.program_status.map(Exit::from_status);
        match origin {
            // The requester knows the session ended: leave nothing behind.
            KillOrigin::Request => {
                self.remove_dir();
                Some(0)
            }
            // SIGTERM from someone else: keep a tombstone.
            KillOrigin::Signal => self.begin_ending(),
        }
    }

    /// Asks a complete model for the screen's tail, and ends once it has
    /// answered, failed, or missed its deadline.
    fn begin_ending(&mut self) -> Option<i32> {
        let snapshot = self.model.request_snapshot(model_mod::TAIL_HISTORY);
        self.model.flush();
        self.ending = Some(Ending {
            snapshot,
            tail: None,
        });
        self.handle_model_events();
        self.finish_ending()
    }

    fn finish_ending(&mut self) -> Option<i32> {
        let e = self.ending.as_mut()?;
        let tail = match (e.snapshot, e.tail.take()) {
            (_, Some(bytes)) => Tail::Screen(bytes),
            (Some(_), None) if self.model.state() == ModelState::Ok => return None,
            _ => Tail::Note(self.model.state()),
        };
        self.tombstone(tail);
        Some(0)
    }

    /// Records `exited`, writes `tail.txt`, removes the socket, and
    /// releases the lock by exiting.
    fn tombstone(&mut self, tail: Tail) {
        self.meta.state = meta::EXITED.into();
        self.meta.ended_at = Some(clock::rfc3339(clock::unix_now()));
        let _ = sys::unlink_at(self.dir.as_fd(), OsStr::new("tail.txt"), false);
        if let Ok(mut f) = sys::open_file_at(
            self.dir.as_fd(),
            OsStr::new("tail.txt"),
            sys::OpenMode::CreateNew,
        ) {
            let bytes = match tail {
                Tail::Screen(bytes) => bytes,
                Tail::Note(state) => tail_note(state).into_bytes(),
            };
            if let Err(e) = f.write_all(&bytes) {
                log(&format!("cannot write tail.txt: {e}"));
            }
        }
        self.write_meta();
        let _ = sys::unlink_at(self.dir.as_fd(), OsStr::new("sock"), false);
        log(&format!(
            "session ended: {}",
            self.meta
                .exit
                .as_ref()
                .map_or_else(|| "unknown".into(), Exit::describe)
        ));
    }

    /// Removes the session directory, if the path still names it.
    fn remove_dir(&mut self) {
        let id = self.meta.id.clone();
        if let Err(e) =
            runtime::remove_locked(self.rt.fd.as_fd(), OsStr::new(&id), self.dir.as_fd())
        {
            log(&format!("cannot remove the session directory: {e}"));
        }
    }

    // -----------------------------------------------------------------
    // Staying published

    /// Checks that the session's path still names the directory this
    /// supervisor holds, and its socket the one it bound. Republishes a
    /// directory that has gone.
    fn check_identity(&mut self) {
        let rt = match runtime::open(&self.cfg_runtime) {
            Ok(rt) => rt,
            Err(e) => {
                log(&format!("runtime directory check failed: {e}"));
                return;
            }
        };
        self.rt = rt;
        let id = OsString::from(&self.meta.id);
        let held = match sys::fstat(self.dir.as_fd()) {
            Ok(st) => st,
            Err(e) => {
                log(&format!("fstat of the session directory: {e}"));
                return;
            }
        };
        match sys::fstatat(self.rt.fd.as_fd(), &id) {
            Ok(st) if st.same_file(&held) => {
                self.conflict_logged = false;
                let ours = sys::fstatat(self.dir.as_fd(), OsStr::new("sock"))
                    .is_ok_and(|s| s.same_file(&self.sock_stat));
                if !ours {
                    self.rebind_socket();
                }
            }
            Ok(_) => {
                if !self.conflict_logged {
                    log(&format!(
                        "{} now names another directory; this session is reachable only with `perch kill --pid`",
                        self.rt.path.join(&id).display()
                    ));
                    self.conflict_logged = true;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.republish(),
            Err(e) => log(&format!("cannot check the session directory: {e}")),
        }
    }

    fn rebind_socket(&mut self) {
        let tmp = format!(".sock-{}", runtime::random_name(8));
        let res = (|| {
            let listener = bind_socket(&self.rt, OsStr::new(&self.meta.id), &tmp)?;
            sys::rename_at(
                self.dir.as_fd(),
                OsStr::new(&tmp),
                self.dir.as_fd(),
                OsStr::new("sock"),
            )?;
            let st = sys::fstatat(self.dir.as_fd(), OsStr::new("sock"))?;
            Ok::<_, io::Error>((listener, st))
        })();
        match res {
            Ok((listener, st)) => {
                self.listener = listener;
                self.sock_stat = st;
                log("socket was missing or replaced; bound a new one");
            }
            Err(e) => {
                let _ = sys::unlink_at(self.dir.as_fd(), OsStr::new(&tmp), false);
                log(&format!("cannot rebind the socket: {e}"));
            }
        }
    }

    /// The directory has gone: build a fresh one with a socket and
    /// metadata, and publish it by the same rename.
    fn republish(&mut self) {
        log("session directory has gone; republishing");
        let (build, dir, listener, sock_stat) = match build_dir(&self.rt) {
            Ok(b) => b,
            Err(e) => {
                log(&format!("cannot rebuild the session directory: {e}"));
                return;
            }
        };
        if !self.meta.degraded.iter().any(|d| d == DIRECTORY_RECREATED) {
            self.meta.degraded.push(DIRECTORY_RECREATED.into());
        }
        let published = meta::write(dir.as_fd(), &self.meta).and_then(|()| {
            sys::rename_noreplace_at(self.rt.fd.as_fd(), &build, OsStr::new(&self.meta.id))
        });
        match published {
            Ok(()) => {
                self.dir = dir;
                self.listener = listener;
                self.sock_stat = sock_stat;
                log(&format!(
                    "republished {}",
                    self.rt.path.join(&self.meta.id).display()
                ));
            }
            Err(e) => {
                let _ = runtime::remove_locked(self.rt.fd.as_fd(), &build, dir.as_fd());
                // build_dir pointed stderr at the new log; point it back.
                if let Ok(f) = sys::open_file_at(
                    self.dir.as_fd(),
                    OsStr::new(LOG_FILE),
                    sys::OpenMode::Append,
                ) {
                    let _ = sys::dup2(f.as_raw_fd(), 2);
                }
                log(&format!("cannot republish: {e}"));
            }
        }
    }

    /// Updates the timestamps of every entry in the session directory,
    /// including other programs' files and sockets, without following
    /// symlinks, so cleaners of old `/tmp` entries leave them alone.
    fn touch_all(&mut self) {
        let _ = sys::touch_at(self.rt.fd.as_fd(), OsStr::new(&self.meta.id));
        touch_tree(self.dir.as_fd(), 0);
    }
}

/// `info`'s name for the model's state.
pub fn model_state_name(state: ModelState) -> &'static str {
    match state {
        ModelState::Ok => "ok",
        ModelState::Partial => "partial",
        ModelState::Failed => "failed",
    }
}

/// What `tail.txt` says when the model could not give the screen.
fn tail_note(state: ModelState) -> String {
    let why = match state {
        ModelState::Partial => {
            "the terminal model was restarted after a failure and had not seen the whole screen"
        }
        _ => "the terminal model had failed",
    };
    format!("[perch] The last screen was not recorded: {why}. See supervisor.log.\n")
}

fn touch_tree(dir: std::os::fd::BorrowedFd<'_>, depth: u32) {
    let Ok(names) = sys::read_dir_names(dir) else {
        return;
    };
    for name in names {
        let _ = sys::touch_at(dir, &name);
        if depth < 8
            && sys::fstatat(dir, &name).is_ok_and(|s| s.is_dir())
            && let Ok(sub) = sys::open_dir_at(dir, &name)
        {
            touch_tree(sub.as_fd(), depth + 1);
        }
    }
}

fn handle_hello(c: &mut Conn, f: &proto::Frame) {
    if f.kind != kind::HELLO {
        return send_error(
            c,
            ErrorBody::new("expected_hello", "the first frame must be Hello"),
        );
    }
    let hello: Hello = match serde_json::from_slice(&f.payload) {
        Ok(h) => h,
        Err(e) => {
            return send_error(
                c,
                ErrorBody::new("bad_hello", format!("unreadable Hello: {e}")),
            );
        }
    };
    let theirs = meta::ProtocolRange {
        min: hello.min,
        max: hello.max,
    };
    let Some(version) = proto::negotiate(proto::PROTOCOL, theirs) else {
        let mut e = ErrorBody::new(
            "version",
            format!(
                "no common protocol version: supervisor speaks {}..{}, client {}..{}",
                proto::PROTOCOL.min,
                proto::PROTOCOL.max,
                hello.min,
                hello.max
            ),
        );
        e.supervisor = Some(proto::PROTOCOL);
        e.client = Some(theirs);
        return send_error(c, e);
    };
    match hello.mode.as_str() {
        "control" => {
            c.phase = Phase::Control;
            let welcome = Welcome {
                protocol: version,
                perch: crate::VERSION.into(),
                capabilities: CAPABILITIES.iter().map(|s| (*s).to_owned()).collect(),
            };
            c.out.extend(proto::encode_json(kind::WELCOME, &welcome));
        }
        "attach" => send_error(
            c,
            ErrorBody::new("unsupported", "this supervisor does not support attach"),
        ),
        other => send_error(
            c,
            ErrorBody::new("bad_hello", format!("unknown mode `{other}`")),
        ),
    }
}

fn send_error(c: &mut Conn, e: ErrorBody) {
    c.out.extend(proto::encode_json(kind::ERROR, &e));
    c.closing = true;
}

fn read_conn(c: &mut Conn) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match c.stream.read(&mut buf) {
            Ok(0) => {
                c.eof = true;
                return;
            }
            Ok(n) => {
                c.decoder.push(&buf[..n]);
                if c.decoder.pending() > proto::MAX_FRAME + 4 + buf.len() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                c.dead = true;
                return;
            }
        }
    }
}

fn flush_conn(c: &mut Conn) {
    while !c.out.is_empty() {
        match c.stream.write(&c.out) {
            Ok(0) => {
                c.dead = true;
                return;
            }
            Ok(n) => {
                c.out.drain(..n);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                c.dead = true;
                return;
            }
        }
    }
}
