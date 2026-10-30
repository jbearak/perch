// SPDX-License-Identifier: GPL-3.0-only

//! `perch attach`'s relay: its own terminal in raw mode, bytes both ways
//! between it and the supervisor, SIGWINCH as resize frames, the detach
//! key, the takeover prompt with re-attach in the same process, and the
//! epilogue when it leaves.
//!
//! It never buffers more than one output frame: it reads the socket only
//! once the last frame is written to its terminal, so a terminal that reads
//! slowly slows the supervisor, which sees the lag. Its terminal and socket
//! are non-blocking both ways, so keystrokes and resizes reach the
//! supervisor while output to the terminal is blocked.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::proto::{
    self, Attach, Attached, Decoder, Detached, ErrorBody, Exited, Frame, Hello, Resize,
    RestoreMode, Welcome, kind,
};
use crate::sys::{self, PollFd};

/// `--if-detached` found a live client.
pub const EXIT_BUSY: u8 = 75;
/// How long the handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(5);
/// The most bytes of input in one frame.
const INPUT_CHUNK: usize = 16 * 1024;
/// Stdin is not read while this much input waits for the socket.
const SOCKET_BACKLOG: usize = 256 * 1024;

/// The key that detaches, as `ctrl-<char>`: its legacy byte and the
/// Unicode code point of the CSI-u forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachKey {
    pub byte: u8,
    pub codepoint: u32,
}

impl DetachKey {
    /// `Ctrl-\`, the dtach and abduco default.
    pub const DEFAULT: DetachKey = DetachKey {
        byte: 0x1c,
        codepoint: b'\\' as u32,
    };

    /// `ctrl-X` or `^X`, X a letter or one of `@ [ \ ] ^ _`, or `none`.
    pub fn parse(s: &str) -> Result<Option<DetachKey>, String> {
        if s == "none" {
            return Ok(None);
        }
        let lower = s.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("ctrl-")
            .or_else(|| lower.strip_prefix("c-"))
            .or_else(|| lower.strip_prefix('^'));
        let c = match rest.map(str::as_bytes) {
            Some([c]) if c.is_ascii_lowercase() || b"@[\\]^_".contains(c) => *c,
            _ => {
                return Err(format!(
                    "--detach-key `{s}`: expected ctrl-<letter>, ctrl-\\, ctrl-], ctrl-^, ctrl-_, ctrl-@, ctrl-[ or none"
                ));
            }
        };
        Ok(Some(DetachKey {
            byte: c & 0x1f,
            codepoint: u32::from(c),
        }))
    }

    /// Where the key is in `b`, and how long its form is: the legacy byte,
    /// the kitty keyboard protocol's `CSI code ; mods u` (with optional
    /// sub-fields, press or repeat events only) or modifyOtherKeys'
    /// `CSI 27 ; mods ; code ~`, with Ctrl and nothing but lock keys.
    pub fn find(&self, b: &[u8]) -> Option<(usize, usize)> {
        let mut i = 0;
        while i < b.len() {
            if b[i] == self.byte {
                return Some((i, 1));
            }
            if b[i] == 0x1b
                && b.get(i + 1) == Some(&b'[')
                && let Some(len) = self.csi_match(&b[i..])
            {
                return Some((i, len));
            }
            i += 1;
        }
        None
    }

    /// The length of `b`'s leading CSI if it is this key.
    fn csi_match(&self, b: &[u8]) -> Option<usize> {
        let end = b[2..].iter().position(|c| (0x40..=0x7e).contains(c))? + 2;
        let params = std::str::from_utf8(&b[2..end]).ok()?;
        let fields: Vec<Vec<&str>> = params.split(';').map(|f| f.split(':').collect()).collect();
        let num = |s: &str| s.parse::<u32>().ok();
        let ctrl = |m: u32| m >= 1 && (m - 1) & !(64 | 128) == 4;
        let hit = match (b[end], &fields[..]) {
            (b'u', [code, mods, ..]) => {
                num(code[0]) == Some(self.codepoint)
                    && num(mods[0]).is_some_and(ctrl)
                    && mods.get(1).is_none_or(|e| matches!(num(e), Some(1 | 2)))
            }
            (b'~', [first, mods, code]) => {
                num(first[0]) == Some(27)
                    && num(mods[0]).is_some_and(ctrl)
                    && num(code[0]) == Some(self.codepoint)
            }
            _ => false,
        };
        hit.then_some(end + 1)
    }
}

/// How long a possible start of the detach key's CSI forms is held, waiting
/// for the rest from a later read: as long as the supervisor holds a split
/// reply (`input::REPLY_HOLD`).
pub const KEY_HOLD: Duration = Duration::from_millis(100);

/// The control-key modifiers of the CSI forms: Ctrl alone, or with Caps Lock
/// or Num Lock.
const CTRL_MODS: [&str; 4] = ["5", "69", "133", "197"];

impl DetachKey {
    /// Whether `s` is a strict prefix of one of the key's CSI forms: it
    /// could still become the key, and is not yet.
    ///
    /// A lone ESC is one. It is held too, under [`KEY_HOLD`], because a key
    /// split right after its ESC must still detach. Terminals send ESC alone
    /// only for the Escape key, which then reaches the program at most
    /// 100 ms late, about what vim's own `ttimeoutlen` waits to tell Escape
    /// from a sequence.
    fn is_prefix(&self, s: &[u8]) -> bool {
        if s.first() != Some(&0x1b) {
            return false;
        }
        if s.len() == 1 {
            return true;
        }
        if s[1] != b'[' || s.len() > 24 {
            return false;
        }
        let Ok(p) = std::str::from_utf8(&s[2..]) else {
            return false;
        };
        if !p
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b';' || c == b':')
        {
            return false;
        }
        let cp = self.codepoint.to_string();
        let f: Vec<&str> = p.split(';').collect();
        let mods_prefix = |m: &str| CTRL_MODS.iter().any(|c| c.starts_with(m));
        let mods_done = |m: &str| CTRL_MODS.contains(&m);
        // Kitty: CSI code[:alternates] ; mods[:event] [; text] u
        let kitty = match &f[..] {
            [code] => {
                let mut subs = code.split(':');
                let first = subs.next().unwrap_or("");
                if code.contains(':') {
                    first == cp
                } else {
                    cp.starts_with(first)
                }
            }
            [code, mods] => {
                code.split(':').next() == Some(cp.as_str()) && {
                    let m: Vec<&str> = mods.split(':').collect();
                    match &m[..] {
                        [m0] => mods_prefix(m0),
                        [m0, ev] => mods_done(m0) && matches!(*ev, "" | "1" | "2"),
                        _ => false,
                    }
                }
            }
            // The associated text: codepoints, `:`-separated.
            [code, mods, _text] => {
                code.split(':').next() == Some(cp.as_str()) && {
                    let m: Vec<&str> = mods.split(':').collect();
                    match &m[..] {
                        [m0] => mods_done(m0),
                        [m0, ev] => mods_done(m0) && matches!(*ev, "1" | "2"),
                        _ => false,
                    }
                }
            }
            _ => false,
        };
        // modifyOtherKeys: CSI 27 ; mods ; code ~
        let mok = !p.contains(':')
            && match &f[..] {
                [a] => "27".starts_with(a),
                [a, m] => *a == "27" && mods_prefix(m),
                [a, m, c] => *a == "27" && mods_done(m) && cp.starts_with(c),
                _ => false,
            };
        kitty || mok
    }
}

/// What the relay does with keystrokes: finds the detach key, holding a
/// possible start of its CSI forms at the end of a read until a later read
/// completes or rules it out, or [`KEY_HOLD`] passes. Bytes are never
/// reordered.
#[derive(Debug)]
pub struct KeyScanner {
    key: Option<DetachKey>,
    held: Vec<u8>,
    deadline: Option<Instant>,
}

/// What a read of keystrokes gives.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scanned {
    /// Bytes for the program, in order.
    pub forward: Vec<u8>,
    /// The detach key came after them.
    pub detach: bool,
}

impl KeyScanner {
    pub fn new(key: Option<DetachKey>) -> KeyScanner {
        KeyScanner {
            key,
            held: Vec::new(),
            deadline: None,
        }
    }

    /// Bytes read from the terminal at `now`.
    pub fn push(&mut self, bytes: &[u8], now: Instant) -> Scanned {
        let Some(key) = self.key else {
            return Scanned {
                forward: bytes.to_vec(),
                detach: false,
            };
        };
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(bytes);
        if let Some((at, _)) = key.find(&data) {
            self.deadline = None;
            data.truncate(at);
            return Scanned {
                forward: data,
                detach: true,
            };
        }
        // Only the last ESC can start a prefix: none may contain another.
        if let Some(j) = data.iter().rposition(|&b| b == 0x1b)
            && key.is_prefix(&data[j..])
        {
            self.held = data.split_off(j);
            // A held start keeps its first deadline.
            if self.deadline.is_none() || j > 0 {
                self.deadline = Some(now + crate::clock::scaled(KEY_HOLD));
            }
        } else {
            self.deadline = None;
        }
        Scanned {
            forward: data,
            detach: false,
        }
    }

    /// Bytes held past their time, now forwarded unchanged.
    pub fn tick(&mut self, now: Instant) -> Vec<u8> {
        if self.deadline.is_some_and(|d| now >= d) {
            self.deadline = None;
            return std::mem::take(&mut self.held);
        }
        Vec::new()
    }

    /// When [`KeyScanner::tick`] has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

/// What `perch attach` does.
#[derive(Debug, Clone)]
pub struct Options {
    pub sock: PathBuf,
    pub label: String,
    /// The first attach's restore.
    pub restore: RestoreMode,
    /// The restore after "Press Enter to take it back".
    pub reattach: RestoreMode,
    pub scrollback: u32,
    pub if_detached: bool,
    pub detach_key: Option<DetachKey>,
    /// Values of the session's linked variables, for its links.
    pub env: std::collections::BTreeMap<String, String>,
}

/// How an attach ended.
enum End {
    /// The user detached, or `perch detach` did.
    Detached,
    /// Another client took over, or this one was too slow: offer to take
    /// it back.
    Offer(&'static str),
    /// The program ended.
    Exited(Exited),
    /// The supervisor went away without a word.
    Lost,
}

/// The terminal's state, put back on drop.
struct Terminal {
    termios: Option<sys::Termios>,
    nonblocking: [Option<bool>; 2],
}

impl Terminal {
    fn raw() -> Terminal {
        let termios = sys::tcgetattr(0);
        if let Some(t) = termios {
            let mut raw = t;
            raw.set_raw();
            let _ = sys::tcsetattr(0, &raw);
        }
        let mut nonblocking = [None, None];
        for fd in 0..2 {
            if let Ok(was) = sys::is_nonblocking(fd) {
                nonblocking[fd as usize] = Some(was);
                let _ = sys::set_nonblocking(fd, true);
            }
        }
        Terminal {
            termios,
            nonblocking,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        for (fd, was) in self.nonblocking.iter().enumerate() {
            if let Some(was) = was {
                let _ = sys::set_nonblocking(fd as i32, *was);
            }
        }
        if let Some(t) = &self.termios {
            let _ = sys::tcsetattr(0, t);
        }
    }
}

/// The terminal's size, clamped, or 80 by 24.
fn terminal_size() -> (u16, u16) {
    let ws = (0..3)
        .find_map(|fd| sys::get_winsize(fd).filter(|w| w.rows > 0 && w.cols > 0))
        .unwrap_or(sys::Winsize { rows: 24, cols: 80 });
    (ws.cols.clamp(1, 500), ws.rows.clamp(1, 500))
}

/// Writes all of `bytes` to the non-blocking stdout.
fn write_terminal(bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match sys::write(1, &bytes[off..]) {
            Ok(n) => off += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let mut fds = [PollFd::new(1, false, true)];
                let _ = sys::poll(&mut fds, Some(1000));
            }
            Err(_) => return,
        }
    }
}

/// Connects, attaches, and relays until the attach ends. Returns the exit
/// status: the program's, 0 after a detach, [`EXIT_BUSY`].
pub fn run(opts: &Options) -> u8 {
    let (sig_r, _sig_w) = match signal_pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("perch: {e}");
            return 1;
        }
    };
    let mut restore = opts.restore;
    let mut if_detached = opts.if_detached;
    let mut terminal: Option<Terminal> = None;
    loop {
        let session = match handshake(opts, restore, if_detached) {
            Ok(Ok(h)) => h,
            Ok(Err(busy)) => {
                drop(terminal);
                eprintln!("perch: {busy}");
                return EXIT_BUSY;
            }
            Err(e) => {
                drop(terminal);
                eprintln!("perch: {e}");
                return 1;
            }
        };
        if terminal.is_none() {
            terminal = Some(Terminal::raw());
        }
        let end = relay(session, &sig_r, opts.detach_key);
        let (cols, _) = terminal_size();
        write_terminal(&crate::restore::epilogue(cols));
        match end {
            End::Detached => {
                write_terminal(b"\r\n[perch] Detached.\r\n");
                return 0;
            }
            End::Lost => {
                write_terminal(b"\r\n[perch] Lost the connection to the session.\r\n");
                return 1;
            }
            End::Exited(e) => {
                drop(terminal);
                match &e.exit.error {
                    Some(err) => eprintln!("perch: {err}"),
                    None => println!("\n[perch] The program ended: {}.", e.exit.describe()),
                }
                let _ = io::stdout().flush();
                return e.status;
            }
            End::Offer(why) => {
                write_terminal(format!("\r\n[perch] {why}\r\n").as_bytes());
                if !wait_for_enter(&sig_r, opts.detach_key) {
                    return 0;
                }
                restore = opts.reattach;
                if_detached = false;
            }
        }
    }
}

/// The self-pipe for SIGWINCH, SIGTERM and SIGHUP.
fn signal_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (r, w) = sys::pipe()?;
    sys::set_nonblocking(r.as_raw_fd(), true)?;
    sys::set_nonblocking(w.as_raw_fd(), true)?;
    sys::route_signals_to_pipe(
        &[sys::sig::WINCH, sys::sig::TERM, sys::sig::HUP],
        w.as_raw_fd(),
    )?;
    Ok((r, w))
}

/// The signals that arrived, by number.
fn take_signals(sig_r: &OwnedFd) -> Vec<i32> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64];
    while let Ok(n) = sys::read(sig_r.as_raw_fd(), &mut buf) {
        if n == 0 {
            break;
        }
        out.extend(buf[..n].iter().map(|&b| i32::from(b)));
    }
    out
}

fn read_frame(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    deadline: Instant,
) -> Result<Frame, String> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(f) = decoder.next_frame().map_err(|e| e.to_string())? {
            return Ok(f);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the session did not answer in time".into());
        }
        // macOS refuses (EINVAL) once the peer has closed; the read then
        // says so.
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut buf) {
            Ok(0) => return Err("the session closed the connection".into()),
            Ok(n) => decoder.push(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn refusal(f: &Frame) -> String {
    let e: ErrorBody = serde_json::from_slice(&f.payload)
        .unwrap_or_else(|_| ErrorBody::new("unknown", "unreadable Error frame"));
    format!("refused ({}): {}", e.code, e.message)
}

/// `Hello`, `Welcome`, `Attach`, then `Attached` (or `Busy`, the inner
/// error). The stream comes back non-blocking, with any bytes already read
/// after `Attached` left in the decoder.
fn handshake(
    opts: &Options,
    restore: RestoreMode,
    if_detached: bool,
) -> Result<Result<Session, String>, String> {
    let deadline = Instant::now() + HANDSHAKE;
    let mut stream = UnixStream::connect(&opts.sock)
        .map_err(|e| format!("cannot connect to the session: {e}"))?;
    stream
        .set_write_timeout(Some(HANDSHAKE))
        .map_err(|e| e.to_string())?;
    let mut decoder = Decoder::new();
    stream
        .write_all(&proto::encode_json(
            kind::HELLO,
            &Hello::attach(&opts.label),
        ))
        .map_err(|e| e.to_string())?;
    let f = read_frame(&mut stream, &mut decoder, deadline)?;
    match f.kind {
        kind::WELCOME => {
            let w: Welcome = serde_json::from_slice(&f.payload)
                .map_err(|e| format!("protocol error: bad Welcome: {e}"))?;
            if !w.capabilities.iter().any(|c| c == "attach") {
                return Err(format!(
                    "the session's supervisor (perch {}) cannot attach",
                    w.perch
                ));
            }
        }
        kind::ERROR => return Err(refusal(&f)),
        k => {
            return Err(format!(
                "protocol error: expected Welcome, got frame type {k}"
            ));
        }
    }
    let (cols, rows) = terminal_size();
    let attach = Attach {
        rows,
        cols,
        restore,
        scrollback: opts.scrollback,
        if_detached,
        env: opts.env.clone(),
        pid: Some(sys::getpid()),
    };
    stream
        .write_all(&proto::encode_json(kind::ATTACH, &attach))
        .map_err(|e| e.to_string())?;
    let f = read_frame(&mut stream, &mut decoder, deadline)?;
    match f.kind {
        kind::ATTACHED => {
            serde_json::from_slice::<Attached>(&f.payload)
                .map_err(|e| format!("protocol error: bad Attached: {e}"))?;
        }
        kind::BUSY => return Ok(Err("the session is attached in another terminal".into())),
        kind::ERROR => return Err(refusal(&f)),
        k => {
            return Err(format!(
                "protocol error: expected Attached, got frame type {k}"
            ));
        }
    }
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    stream.set_write_timeout(None).map_err(|e| e.to_string())?;
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(Ok(Session { stream, decoder }))
}

struct Session {
    stream: UnixStream,
    decoder: Decoder,
}

/// Relays until the attach ends.
fn relay(session: Session, sig_r: &OwnedFd, key: Option<DetachKey>) -> End {
    let Session {
        stream,
        mut decoder,
    } = session;
    // One output frame's payload, and how much of it is written.
    let mut out: Vec<u8> = Vec::new();
    let mut out_off = 0;
    // Frames for the socket.
    let mut to_sock: Vec<u8> = Vec::new();
    let mut stdin_open = true;
    let mut sock_open = true;
    let mut detaching = false;
    let mut keys = KeyScanner::new(key);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // Take the next frame once the last is written.
        while out_off == out.len() {
            out.clear();
            out_off = 0;
            match decoder.next_frame() {
                Ok(Some(f)) => match f.kind {
                    kind::OUTPUT => out = f.payload,
                    kind::EXITED => {
                        return match serde_json::from_slice::<Exited>(&f.payload) {
                            Ok(e) => End::Exited(e),
                            Err(_) => End::Lost,
                        };
                    }
                    kind::DETACHED => {
                        let reason = serde_json::from_slice::<Detached>(&f.payload)
                            .map(|d| d.reason)
                            .unwrap_or_default();
                        return match reason.as_str() {
                            "takeover" => End::Offer(
                                "Opened in another window. Press Enter to take it back, or close this terminal.",
                            ),
                            "too_slow" => End::Offer(
                                "This terminal could not keep up. Press Enter to attach again, or close this terminal.",
                            ),
                            _ => End::Detached,
                        };
                    }
                    _ => {}
                },
                Ok(None) => break,
                Err(_) => return End::Lost,
            }
        }
        if !sock_open && out_off == out.len() {
            return if detaching { End::Detached } else { End::Lost };
        }
        if detaching && to_sock.is_empty() {
            // Everything typed before the key has gone: closing our side
            // detaches, after the supervisor has read it.
            let _ = stream.shutdown(Shutdown::Write);
            return End::Detached;
        }

        let mut fds = vec![
            PollFd::new(sig_r.as_raw_fd(), true, false),
            PollFd::new(
                stream.as_raw_fd(),
                sock_open && out_off == out.len(),
                !to_sock.is_empty(),
            ),
            PollFd::new(1, false, out_off < out.len()),
        ];
        let read_stdin = stdin_open && !detaching && to_sock.len() < SOCKET_BACKLOG;
        fds.push(PollFd::new(0, read_stdin, false));
        let timeout = keys.deadline().map(|d| {
            let left = d.saturating_duration_since(Instant::now());
            left.as_millis() as u64 + u64::from(left.subsec_nanos() % 1_000_000 != 0)
        });
        if sys::poll(&mut fds, timeout).is_err() {
            continue;
        }
        // A held start of the detach key whose time is up.
        let late = keys.tick(Instant::now());
        if !late.is_empty() {
            to_sock.extend(proto::encode(kind::INPUT, &late).expect("held bytes are few"));
        }

        for sig in take_signals(sig_r) {
            if sig == sys::sig::WINCH {
                let (cols, rows) = terminal_size();
                to_sock.extend(proto::encode_json(kind::RESIZE, &Resize { rows, cols }));
            } else if sig == sys::sig::TERM || sig == sys::sig::HUP {
                // The terminal is closing: closing the socket detaches.
                return End::Detached;
            }
        }

        // Keystrokes.
        if read_stdin && (fds[3].readable || fds[3].hangup) {
            match sys::read(0, &mut buf[..INPUT_CHUNK]) {
                Ok(0) => {
                    stdin_open = false;
                    let rest = keys.tick(Instant::now() + crate::clock::scaled(KEY_HOLD));
                    if !rest.is_empty() {
                        to_sock.extend(proto::encode(kind::INPUT, &rest).expect("few"));
                    }
                }
                Ok(n) => {
                    let scanned = keys.push(&buf[..n], Instant::now());
                    detaching = scanned.detach;
                    if !scanned.forward.is_empty() {
                        to_sock.extend(
                            proto::encode(kind::INPUT, &scanned.forward)
                                .expect("input chunks are small"),
                        );
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => stdin_open = false,
            }
        }

        // To the socket.
        while !to_sock.is_empty() {
            match (&stream).write(&to_sock) {
                Ok(0) => break,
                Ok(n) => {
                    to_sock.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    to_sock.clear();
                    sock_open = false;
                    break;
                }
            }
        }

        // To the terminal.
        while out_off < out.len() {
            match sys::write(1, &out[out_off..]) {
                Ok(n) => out_off += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // The terminal is gone.
                Err(_) => return End::Detached,
            }
        }

        // From the socket, only once the last frame is written.
        if sock_open && out_off == out.len() && (fds[1].readable || fds[1].hangup) {
            match (&stream).read(&mut buf) {
                Ok(0) => sock_open = false,
                Ok(n) => decoder.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => sock_open = false,
            }
        }
    }
}

/// Waits for Enter. Returns false if the user closed the terminal or
/// pressed the detach key or Ctrl-C instead.
fn wait_for_enter(sig_r: &OwnedFd, key: Option<DetachKey>) -> bool {
    let mut buf = [0u8; 256];
    loop {
        let mut fds = [
            PollFd::new(sig_r.as_raw_fd(), true, false),
            PollFd::new(0, true, false),
        ];
        if sys::poll(&mut fds, None).is_err() {
            continue;
        }
        if take_signals(sig_r)
            .iter()
            .any(|&s| s == sys::sig::TERM || s == sys::sig::HUP)
        {
            return false;
        }
        if !(fds[1].readable || fds[1].hangup) {
            continue;
        }
        match sys::read(0, &mut buf) {
            Ok(0) => return false,
            Ok(n) => {
                let data = &buf[..n];
                if data.contains(&0x03) || key.is_some_and(|k| k.find(data).is_some()) {
                    return false;
                }
                if data.iter().any(|&b| b == b'\r' || b == b'\n') {
                    return true;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_key_forms() {
        let k = DetachKey::DEFAULT;
        assert_eq!(k.find(b"ab\x1ccd"), Some((2, 1)));
        // kitty keyboard protocol, with and without event types and lock
        // keys.
        assert_eq!(k.find(b"x\x1b[92;5u"), Some((1, 7)));
        assert_eq!(k.find(b"\x1b[92;5:1u"), Some((0, 9)));
        assert_eq!(k.find(b"\x1b[92:124;69u"), Some((0, 12)));
        assert_eq!(k.find(b"\x1b[92;5:3u"), None, "a release");
        assert_eq!(k.find(b"\x1b[92;6u"), None, "ctrl-shift");
        assert_eq!(k.find(b"\x1b[92u"), None, "no ctrl");
        assert_eq!(k.find(b"\x1b[93;5u"), None);
        // modifyOtherKeys.
        assert_eq!(k.find(b"\x1b[27;5;92~"), Some((0, 10)));
        assert_eq!(k.find(b"\x1b[27;5;93~"), None);
        assert_eq!(k.find(b"\x1b[A\x1b["), None);
    }

    /// Feeds `parts` as separate reads, 10 ms apart, and returns what was
    /// forwarded, whether it detached, and what is still held.
    fn scan(key: Option<DetachKey>, parts: &[&[u8]]) -> (Vec<u8>, bool, Vec<u8>) {
        let mut k = KeyScanner::new(key);
        let t0 = Instant::now();
        let mut out = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            let now = t0 + Duration::from_millis(10 * i as u64);
            out.extend(k.tick(now));
            let s = k.push(p, now);
            out.extend(s.forward);
            if s.detach {
                return (out, true, k.held);
            }
        }
        (out, false, k.held)
    }

    #[test]
    fn every_split_of_the_csi_forms_detaches() {
        let k = Some(DetachKey::DEFAULT);
        for form in [
            &b"\x1b[92;5u"[..],
            b"\x1b[92;5:1u",
            b"\x1b[92;5;92u",
            b"\x1b[27;5;92~",
        ] {
            for cut in 1..form.len() {
                let mut first = b"ab".to_vec();
                first.extend_from_slice(&form[..cut]);
                let (out, detach, _) = scan(k, &[&first, &form[cut..]]);
                assert!(detach, "{form:?} cut at {cut}");
                assert_eq!(out, b"ab", "{form:?} cut at {cut}");
            }
            // In three reads, every pair of cuts.
            for a in 1..form.len() {
                for b in a + 1..form.len() {
                    let (out, detach, _) = scan(k, &[&form[..a], &form[a..b], &form[b..]]);
                    assert!(detach && out.is_empty(), "{form:?} cut at {a}, {b}");
                }
            }
        }
    }

    #[test]
    fn escape_and_arrows_are_forwarded_in_order_within_the_hold() {
        let k = Some(DetachKey::DEFAULT);
        // Escape alone: held, then forwarded unchanged once the hold ends.
        let mut s = KeyScanner::new(k);
        let t0 = Instant::now();
        assert_eq!(s.push(b"\x1b", t0).forward, b"");
        assert_eq!(s.deadline(), Some(t0 + KEY_HOLD));
        assert_eq!(s.tick(t0 + KEY_HOLD - Duration::from_millis(1)), b"");
        assert_eq!(s.tick(t0 + KEY_HOLD), b"\x1b");
        assert_eq!(s.deadline(), None);
        // Escape then a key: forwarded at once, in order.
        assert_eq!(
            scan(k, &[b"\x1b", b"j"]),
            (b"\x1bj".to_vec(), false, vec![])
        );
        // An arrow key split at every point.
        for arrow in [&b"\x1b[A"[..], b"\x1bOA", b"\x1b[1;5C"] {
            for cut in 1..arrow.len() {
                let (out, detach, held) = scan(k, &[&arrow[..cut], &arrow[cut..], b"x"]);
                assert!(!detach);
                assert!(held.is_empty(), "{arrow:?} at {cut}");
                let mut want = arrow.to_vec();
                want.push(b'x');
                assert_eq!(out, want, "{arrow:?} at {cut}");
            }
        }
        // A held start that keeps growing keeps its first deadline.
        let mut s = KeyScanner::new(k);
        s.push(b"\x1b[", t0);
        s.push(b"9", t0 + Duration::from_millis(60));
        assert_eq!(s.tick(t0 + KEY_HOLD), b"\x1b[9");
    }

    #[test]
    fn only_strict_prefixes_are_held() {
        let k = DetachKey::DEFAULT;
        for p in [
            &b"\x1b"[..],
            b"\x1b[",
            b"\x1b[9",
            b"\x1b[92",
            b"\x1b[92;",
            b"\x1b[92;5",
            b"\x1b[92;5:",
            b"\x1b[92:124;6",
            b"\x1b[2",
            b"\x1b[27;",
            b"\x1b[27;5;9",
        ] {
            assert!(k.is_prefix(p), "{p:?}");
        }
        for p in [
            &b"x"[..],
            b"\x1bO",
            b"\x1b[A",
            b"\x1b[1",
            b"\x1b[93",
            b"\x1b[92;7",
            b"\x1b[92;5:3",
            b"\x1b[27;5;8",
            b"\x1b[27;7",
            b"\x1b[92;5u",
        ] {
            assert!(!k.is_prefix(p), "{p:?}");
        }
        // With the key off, nothing is held.
        assert_eq!(
            scan(None, &[b"\x1b[92"]),
            (b"\x1b[92".to_vec(), false, vec![])
        );
    }

    #[test]
    fn detach_key_names() {
        assert_eq!(DetachKey::parse("none").unwrap(), None);
        assert_eq!(
            DetachKey::parse("ctrl-\\").unwrap(),
            Some(DetachKey::DEFAULT)
        );
        assert_eq!(
            DetachKey::parse("^A").unwrap(),
            Some(DetachKey {
                byte: 1,
                codepoint: u32::from(b'a')
            })
        );
        assert_eq!(
            DetachKey::parse("ctrl-]").unwrap().map(|k| k.byte),
            Some(0x1d)
        );
        assert!(DetachKey::parse("ctrl-1").is_err());
        assert!(DetachKey::parse("x").is_err());
    }
}
