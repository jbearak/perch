// SPDX-License-Identifier: GPL-3.0-only

//! Clients for the attach tests: [`Pty`] runs `perch` with a pseudo-terminal
//! of the test's own as its controlling terminal, and [`Recorder`] speaks
//! attach mode itself and records what the supervisor sends.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use perch::proto::{self, Decoder, Frame, kind};
use perch::sys;
use serde_json::{Value, json};

use super::{Rt, perch_bin};

/// Whether `needle` occurs in `hay`.
pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// Where `needle` first occurs in `hay`.
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `perch` on a terminal of the test's own.
pub struct Pty {
    master: std::fs::File,
    /// Kept open so the terminal's size can be set through it (macOS sets
    /// it only through a terminal whose slave is open).
    slave: OwnedFd,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    /// While set, nothing reads the terminal: `perch` blocks writing to it.
    paused: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Pty {
    /// Runs `perch args` against `rt` in a new session whose controlling
    /// terminal is a new PTY of `cols` by `rows`.
    pub fn spawn(rt: &Rt, args: &[&str], cols: u16, rows: u16) -> Pty {
        Pty::spawn_env(rt, args, cols, rows, &[])
    }

    pub fn spawn_env(rt: &Rt, args: &[&str], cols: u16, rows: u16, env: &[(&str, &str)]) -> Pty {
        let (master, slave_path) = sys::open_pty().expect("open a PTY");
        let slave = sys::open_rw_noctty(&slave_path).expect("open the slave");
        sys::set_winsize(slave.as_raw_fd(), sys::Winsize { rows, cols }).expect("size");
        let stdio = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup"));
        let mut cmd = Command::new(perch_bin());
        cmd.args(args)
            .env("PERCH_RUNTIME_DIR", &rt.path)
            .env("PERCH_TEST_TIME_SCALE", &rt.scale)
            .env_remove("PERCH_SESSION_ID")
            .env_remove("SSH_AUTH_SOCK")
            .env("TERM", "xterm-256color")
            .stdin(stdio(&slave))
            .stdout(stdio(&slave))
            .stderr(stdio(&slave));
        for (k, v) in env {
            cmd.env(k, v);
        }
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                sys::setsid()?;
                sys::set_controlling_tty(0)?;
                Ok(())
            });
        }
        let child = cmd.spawn().expect("run perch");
        let master = std::fs::File::from(master);
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut r = master.try_clone().expect("dup the master");
        let out = Arc::clone(&output);
        let paused = Arc::new(AtomicBool::new(false));
        let p = Arc::clone(&paused);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                if p.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => out.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
        });
        Pty {
            master,
            slave,
            child,
            output,
            paused,
            reader: Some(reader),
        }
    }

    /// Stops reading the terminal, or starts again. A read already waiting
    /// takes one more chunk first.
    pub fn pause(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
    }

    /// Types `bytes` into the terminal.
    pub fn type_bytes(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("type");
    }

    /// Everything the terminal has shown so far.
    pub fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    pub fn output_len(&self) -> usize {
        self.output.lock().unwrap().len()
    }

    /// Waits until the output after `from` contains `needle`.
    pub fn wait_output(&self, from: usize, needle: &[u8], timeout: Duration) -> bool {
        super::wait_for(timeout, || {
            let out = self.output.lock().unwrap();
            contains(&out[from.min(out.len())..], needle).then_some(())
        })
        .is_some()
    }

    /// Like [`Pty::wait_output`], panicking with the output on timeout.
    pub fn expect(&self, from: usize, needle: &[u8]) {
        assert!(
            self.wait_output(from, needle, Duration::from_secs(15)),
            "{:?} never appeared; the terminal shows {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&self.output()[from.min(self.output_len())..])
        );
    }

    /// Resizes the terminal, which sends `perch` SIGWINCH.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        sys::set_winsize(self.slave.as_raw_fd(), sys::Winsize { rows, cols }).expect("resize");
    }

    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Waits for `perch` to exit.
    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(s) = self.child.try_wait().expect("wait") {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits for `perch` to exit and returns its code.
    pub fn exit_code(&mut self) -> i32 {
        let status = self.wait(Duration::from_secs(15)).unwrap_or_else(|| {
            panic!(
                "perch did not exit; the terminal shows {:?}",
                String::from_utf8_lossy(&self.output())
            )
        });
        status.code().unwrap_or(-1)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The reader is left blocked on the master; it ends with the test.
        drop(self.reader.take());
    }
}

/// An attach-mode client that records every frame.
pub struct Recorder {
    stream: UnixStream,
    decoder: Decoder,
    /// Every output byte, in order.
    pub output: Vec<u8>,
    /// Every other frame, in order.
    pub frames: Vec<Frame>,
    pub closed: bool,
}

impl Recorder {
    /// Connects to session `id` and attaches with `attach` (merged over
    /// 80 by 24, `screen`). Returns the first answer frame too.
    pub fn attach(rt: &Rt, id: &str, attach: Value) -> (Recorder, Frame) {
        Recorder::attach_sock(&rt.dir(id).join("sock"), attach)
    }

    pub fn attach_sock(sock: &Path, attach: Value) -> (Recorder, Frame) {
        let stream = UnixStream::connect(sock).expect("connect");
        let mut r = Recorder {
            stream,
            decoder: Decoder::new(),
            output: Vec::new(),
            frames: Vec::new(),
            closed: false,
        };
        r.send(
            kind::HELLO,
            &serde_json::to_vec(&proto::Hello::attach("test-recorder")).unwrap(),
        );
        let w = r.next_frame(Duration::from_secs(5)).expect("Welcome");
        assert_eq!(
            w.kind,
            kind::WELCOME,
            "{}",
            String::from_utf8_lossy(&w.payload)
        );
        let mut req = json!({"rows": 24, "cols": 80, "restore": "screen", "scrollback": 1000});
        for (k, v) in attach.as_object().into_iter().flatten() {
            req[k] = v.clone();
        }
        r.send(kind::ATTACH, &serde_json::to_vec(&req).unwrap());
        let answer = r.next_frame(Duration::from_secs(5)).expect("an answer");
        (r, answer)
    }

    fn send(&mut self, k: u8, payload: &[u8]) {
        self.stream
            .write_all(&proto::encode(k, payload).unwrap())
            .expect("send");
    }

    pub fn input(&mut self, bytes: &[u8]) {
        self.send(kind::INPUT, bytes);
    }

    /// Input that may find the connection already closed: a terminal's
    /// late answer after the supervisor let it go.
    pub fn late_input(&mut self, bytes: &[u8]) {
        let _ = self
            .stream
            .write_all(&proto::encode(kind::INPUT, bytes).unwrap());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.send(
            kind::RESIZE,
            &serde_json::to_vec(&json!({"rows": rows, "cols": cols})).unwrap(),
        );
    }

    /// Closes the connection, which detaches.
    pub fn close(self) {}

    /// Reads one raw frame, without recording it.
    fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        let end = Instant::now() + timeout;
        let mut buf = [0u8; 65536];
        loop {
            if let Some(f) = self.decoder.next_frame().expect("valid frames") {
                return Some(f);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() || self.closed {
                return None;
            }
            // macOS refuses (EINVAL) once the peer has closed; the read
            // then says so.
            let _ = self.stream.set_read_timeout(Some(left));
            match self.stream.read(&mut buf) {
                Ok(0) => self.closed = true,
                Ok(n) => self.decoder.push(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => self.closed = true,
            }
        }
    }

    /// Records frames until `done` holds, or `timeout` passes. Returns
    /// whether it held.
    pub fn until(&mut self, timeout: Duration, mut done: impl FnMut(&Recorder) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if done(self) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.next_frame(left.min(Duration::from_millis(50))) {
                Some(f) if f.kind == kind::OUTPUT => self.output.extend_from_slice(&f.payload),
                Some(f) => self.frames.push(f),
                None if self.closed => return done(self),
                None => {}
            }
        }
    }

    /// Records until the output contains `needle`.
    pub fn expect(&mut self, needle: &[u8]) {
        let ok = self.until(Duration::from_secs(15), |r| contains(&r.output, needle));
        assert!(
            ok,
            "{:?} never arrived; got {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&self.output)
        );
    }

    /// Reads at most `max` bytes from the socket, waiting up to `timeout`,
    /// and records the frames they complete: a client that reads slowly.
    pub fn read_some(&mut self, max: usize, timeout: Duration) {
        let mut buf = vec![0u8; max];
        let _ = self.stream.set_read_timeout(Some(timeout));
        match self.stream.read(&mut buf) {
            Ok(0) => self.closed = true,
            Ok(n) => self.decoder.push(&buf[..n]),
            Err(_) => {}
        }
        while let Some(f) = self.decoder.next_frame().expect("valid frames") {
            match f.kind {
                kind::OUTPUT => self.output.extend_from_slice(&f.payload),
                _ => self.frames.push(f),
            }
        }
    }

    /// Records whatever arrives within `quiet` of the last frame.
    pub fn settle(&mut self, quiet: Duration) {
        loop {
            match self.next_frame(quiet) {
                Some(f) if f.kind == kind::OUTPUT => self.output.extend_from_slice(&f.payload),
                Some(f) => self.frames.push(f),
                None => return,
            }
        }
    }

    /// The first recorded frame of `k`, as JSON.
    pub fn frame(&self, k: u8) -> Option<Value> {
        self.frames
            .iter()
            .find(|f| f.kind == k)
            .map(|f| serde_json::from_slice(&f.payload).expect("JSON frame"))
    }
}
