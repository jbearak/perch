// SPDX-License-Identifier: GPL-3.0-only

//! `proc-state OUT`: writes what a program inherits as JSON to OUT, then
//! waits for a signal: umask, working directory, blocked signals, signals
//! with a non-default disposition, the terminal's size and termios flags,
//! whether it is a session leader, and the environment.
//!
//! Rust's runtime ignores SIGPIPE before `main`, so the signal state is
//! captured earlier, by a constructor.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

static BLOCKED: AtomicU64 = AtomicU64::new(0);
static NON_DEFAULT: AtomicU64 = AtomicU64::new(0);

fn mask(sigs: &[i32]) -> u64 {
    sigs.iter()
        .filter(|&&s| (1..64).contains(&s))
        .fold(0, |m, &s| m | 1 << s)
}

extern "C" fn capture() {
    BLOCKED.store(mask(&perch::sys::blocked_signals()), Ordering::Relaxed);
    NON_DEFAULT.store(mask(&perch::sys::non_default_signals()), Ordering::Relaxed);
}

#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
static CAPTURE: extern "C" fn() = capture;

fn signals(m: u64) -> Vec<i32> {
    (1..64).filter(|s| m & (1 << s) != 0).collect()
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: proc-state OUT");
    let size = perch::sys::get_winsize(0).unwrap_or(perch::sys::Winsize { rows: 0, cols: 0 });
    let termios = perch::sys::tcgetattr(0);
    let pid = perch::sys::getpid();
    let env: serde_json::Map<String, serde_json::Value> =
        std::env::vars().map(|(k, v)| (k, json!(v))).collect();
    let state = json!({
        "pid": pid,
        "umask": perch::sys::current_umask(),
        "cwd": std::env::current_dir().ok(),
        "blocked": signals(BLOCKED.load(Ordering::Relaxed)),
        "nonDefault": signals(NON_DEFAULT.load(Ordering::Relaxed)),
        "rows": size.rows,
        "cols": size.cols,
        "isatty": perch::sys::isatty(0),
        "iutf8": termios.as_ref().map(perch::sys::Termios::iutf8),
        "echo": termios.as_ref().map(perch::sys::Termios::echo),
        "icanon": termios.as_ref().map(perch::sys::Termios::icanon),
        "sessionLeader": perch::sys::getsid(0).ok() == Some(pid),
        "env": env,
    });
    perch_testprogs::write_atomically(std::path::Path::new(&out), &format!("{state:#}\n"));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
