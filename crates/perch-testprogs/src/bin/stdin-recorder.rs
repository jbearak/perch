// SPDX-License-Identifier: GPL-3.0-only

//! `stdin-recorder OUT [--flood]`: puts its terminal in raw mode, says
//! `recording`, and appends every byte it reads from stdin to OUT as it
//! arrives. The no-injection test compares OUT with what was typed: Perch
//! must never add a byte of its own. With `--flood` it also writes lines of
//! output without end, for tests of input under backpressure.

use std::io::Write;

use perch::sys;

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: stdin-recorder OUT [--flood]");
    let flood = std::env::args().nth(2).is_some_and(|a| a == "--flood");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .expect("open OUT");
    if let Some(mut t) = sys::tcgetattr(0) {
        t.set_raw();
        sys::tcsetattr(0, &t).expect("raw mode");
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(b"recording\r\n").expect("write");
    stdout.flush().expect("flush");
    drop(stdout);
    if flood {
        std::thread::spawn(|| {
            let mut out = std::io::stdout().lock();
            for n in 0u64.. {
                if writeln!(out, "flood line {n:>12} 0123456789abcdef0123456789abcdef\r").is_err() {
                    return;
                }
            }
        });
    }
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(0, &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                file.write_all(&buf[..n]).expect("record");
                file.flush().expect("flush");
            }
        }
    }
}
