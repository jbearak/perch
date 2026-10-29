// SPDX-License-Identifier: GPL-3.0-only

//! `dsr-timer OUT GO [TIMEOUT_MS]`: sends terminal queries in rounds and
//! times the replies, with its terminal in raw mode.
//!
//! It waits for the file GO, reads the words in it, and deletes it. Each
//! word is one step:
//!
//! - `dsr5`, `dsr6`, `da1`, `da2`: send the query and wait up to
//!   TIMEOUT_MS (default 1000) for a reply;
//! - `ris`: write RIS;
//! - `flood:N`: write N bytes of text;
//! - `text:WORD`: write WORD and a newline;
//! - `exit:N`: exit with status N.
//!
//! After each round it rewrites OUT with one JSON line per round so far:
//! an array of `{"step", "reply", "ms"}`, `reply` null when none came.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use perch::sys::{self, PollFd};
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (out, go) = match &args[..] {
        [out, go, ..] => (Path::new(out), Path::new(go)),
        _ => panic!("usage: dsr-timer OUT GO [TIMEOUT_MS]"),
    };
    let timeout = Duration::from_millis(args.get(2).map_or(1000, |t| t.parse().expect("ms")));
    if let Some(mut t) = sys::tcgetattr(0) {
        t.set_raw();
        sys::tcsetattr(0, &t).expect("raw mode");
    }
    let mut rounds: Vec<String> = Vec::new();
    loop {
        let words = loop {
            if let Ok(text) = std::fs::read_to_string(go) {
                let _ = std::fs::remove_file(go);
                break text;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut results = Vec::new();
        for word in words.split_whitespace() {
            results.push(step(word, timeout));
        }
        rounds.push(Value::Array(results).to_string());
        let mut text = rounds.join("\n");
        text.push('\n');
        perch_testprogs::write_atomically(out, &text);
    }
}

fn step(word: &str, timeout: Duration) -> Value {
    let (query, last): (&[u8], u8) = match word {
        "dsr5" => (b"\x1b[5n", b'n'),
        "dsr6" => (b"\x1b[6n", b'R'),
        "da1" => (b"\x1b[c", b'c'),
        "da2" => (b"\x1b[>c", b'c'),
        "ris" => {
            write(b"\x1bc");
            return json!({ "step": word });
        }
        _ => {
            if let Some(n) = word.strip_prefix("flood:") {
                let n: usize = n.parse().expect("flood size");
                let line = [b"0123456789abcdef".repeat(4).as_slice(), b"\r\n"].concat();
                let block: Vec<u8> = line.iter().copied().cycle().take(64 * 1024).collect();
                let mut left = n;
                while left > 0 {
                    let k = left.min(block.len());
                    write(&block[..k]);
                    left -= k;
                }
            } else if let Some(n) = word.strip_prefix("longcsi:") {
                // A CSI left unfinished after `n` parameter digits.
                let n: usize = n.parse().expect("longcsi size");
                write(b"\x1b[");
                let block = vec![b'1'; 64 * 1024];
                let mut left = n;
                while left > 0 {
                    let k = left.min(block.len());
                    write(&block[..k]);
                    left -= k;
                }
            } else if let Some(t) = word.strip_prefix("text:") {
                write(format!("{t}\r\n").as_bytes());
            } else if let Some(code) = word.strip_prefix("exit:") {
                std::process::exit(code.parse().expect("exit code"));
            } else {
                panic!("unknown step `{word}`");
            }
            return json!({ "step": word });
        }
    };
    // Whatever arrived late for an earlier query is not this reply.
    let mut stale = Vec::new();
    read_until(&mut stale, None, Duration::ZERO);
    let start = Instant::now();
    write(query);
    let mut reply = Vec::new();
    let got = read_until(&mut reply, Some(last), timeout);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    json!({
        "step": word,
        "reply": got.then(|| String::from_utf8_lossy(&reply).into_owned()),
        "ms": ms,
    })
}

fn write(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes).expect("write");
    out.flush().expect("flush");
}

/// Reads stdin into `buf` until a byte equal to `last` arrives or
/// `timeout` passes. Returns whether `last` arrived.
fn read_until(buf: &mut Vec<u8>, last: Option<u8>, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let mut fds = [PollFd::new(0, true, false)];
        if sys::poll(&mut fds, Some(left.as_millis() as u64)).is_err() {
            return false;
        }
        if !fds[0].readable {
            if Instant::now() >= end {
                return false;
            }
            continue;
        }
        let mut b = [0u8; 256];
        match sys::read(0, &mut b) {
            Ok(0) | Err(_) => return false,
            Ok(n) => {
                buf.extend_from_slice(&b[..n]);
                if last.is_some_and(|l| buf.contains(&l)) {
                    return true;
                }
            }
        }
    }
}
