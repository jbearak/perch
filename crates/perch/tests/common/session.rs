// SPDX-License-Identifier: GPL-3.0-only

//! Driving a session from a test: `dsr-timer` rounds, control requests and
//! the supervisor's test hooks, and `perch send`.

use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

use perch::client::Client;
use serde_json::Value;

use super::{Rt, testprog, wait_for};

pub const SECS: Duration = Duration::from_secs(15);

/// A control connection to session `id`.
pub fn control(rt: &Rt, id: &str) -> Client {
    Client::connect(&rt.dir(id).join("sock"), "test", SECS).expect("connect")
}

/// A control request that must succeed.
pub fn request(rt: &Rt, id: &str, op: &str, fields: Value) -> Value {
    control(rt, id)
        .request(op, fields, SECS)
        .unwrap_or_else(|e| panic!("{op}: {e}"))
}

pub fn info(rt: &Rt, id: &str) -> Value {
    request(rt, id, "info", serde_json::json!({}))
}

/// The input queue's state, after setting the gate if `gate` is given:
/// `Some(Some(n))` lets `n` more bytes through, `Some(None)` opens it.
pub fn input_hook(rt: &Rt, id: &str, gate: Option<Option<usize>>) -> Value {
    let fields = match gate {
        None => serde_json::json!({}),
        Some(g) => serde_json::json!({ "gate": g }),
    };
    request(rt, id, "test-input", fields)
}

/// Waits until the input hook's state satisfies `f`.
pub fn wait_input(rt: &Rt, id: &str, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    wait_for(SECS, || {
        let v = input_hook(rt, id, None);
        f(&v).then_some(v)
    })
    .unwrap_or_else(|| {
        panic!(
            "the input queue never got {what}: {}",
            input_hook(rt, id, None)
        )
    })
}

/// `perch send <id> args...`, with `stdin` piped in if given.
pub fn send(rt: &Rt, id: &str, args: &[&str], stdin: Option<&[u8]>) -> Output {
    send_env(rt, id, args, stdin, &[])
}

pub fn send_env(
    rt: &Rt,
    id: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(&str, &str)],
) -> Output {
    use std::io::Write;
    let mut c = rt.cmd();
    c.arg("send").arg(id).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    if stdin.is_some() {
        c.stdin(std::process::Stdio::piped());
    }
    let mut child = c.spawn().expect("run perch send");
    if let Some(bytes) = stdin {
        let mut w = child.stdin.take().unwrap();
        w.write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// `perch send` that must succeed.
pub fn send_ok(rt: &Rt, id: &str, args: &[&str]) {
    let out = send(rt, id, args, None);
    assert!(
        out.status.success(),
        "perch send {args:?}: {}",
        super::describe(&out)
    );
}

/// `dsr-timer` in a session: rounds of steps written to its GO file.
pub struct Timer {
    pub out: PathBuf,
    go: PathBuf,
    rounds: usize,
}

impl Timer {
    /// The command for `perch new`, with a query timeout of `timeout_ms`.
    pub fn command(timeout_ms: u64) -> Vec<String> {
        vec![
            testprog("dsr-timer").to_str().unwrap().to_owned(),
            "{dir}/timer.out".into(),
            "{dir}/timer.go".into(),
            timeout_ms.to_string(),
        ]
    }

    /// Starts a detached session running `dsr-timer`.
    pub fn new_session(rt: &Rt, extra: &[&str], timeout_ms: u64) -> (String, Timer) {
        let cmd = Timer::command(timeout_ms);
        let mut args: Vec<&str> = extra.to_vec();
        args.push("--");
        args.extend(cmd.iter().map(String::as_str));
        let id = rt.new_session(&args);
        let t = Timer {
            out: rt.dir(&id).join("timer.out"),
            go: rt.dir(&id).join("timer.go"),
            rounds: 0,
        };
        (id, t)
    }

    /// Starts a round without waiting for it.
    pub fn start(&mut self, words: &str) {
        let tmp = self.go.with_extension("tmp");
        std::fs::write(&tmp, words).unwrap();
        std::fs::rename(&tmp, &self.go).unwrap();
        self.rounds += 1;
    }

    /// Waits for the round started last and returns its steps.
    pub fn wait(&self) -> Vec<Value> {
        let want = self.rounds;
        let text = wait_for(Duration::from_secs(90), || {
            let text = std::fs::read_to_string(&self.out).ok()?;
            (text.lines().count() >= want).then_some(text)
        })
        .unwrap_or_else(|| panic!("round {want} never finished"));
        let line = text.lines().nth(want - 1).unwrap();
        serde_json::from_str::<Value>(line)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    /// Runs a round and returns its steps.
    pub fn round(&mut self, words: &str) -> Vec<Value> {
        self.start(words);
        self.wait()
    }
}

/// The bytes `dsr-timer`'s `flood:N` writes.
pub fn flood_bytes(n: usize) -> Vec<u8> {
    let line = [b"0123456789abcdef".repeat(4).as_slice(), b"\r\n"].concat();
    let block: Vec<u8> = line.iter().copied().cycle().take(64 * 1024).collect();
    block.iter().copied().cycle().take(n).collect()
}
