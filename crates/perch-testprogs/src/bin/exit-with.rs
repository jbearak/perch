// SPDX-License-Identifier: GPL-3.0-only

//! `exit-with CODE [DELAY_MS]` exits with CODE after DELAY_MS;
//! `exit-with signal NAME [DELAY_MS]` kills itself with SIGKILL, SIGTERM,
//! SIGINT or SIGHUP.

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let delay = |i: usize| {
        args.get(i)
            .and_then(|d| d.parse().ok())
            .map_or(Duration::ZERO, Duration::from_millis)
    };
    if args.first().map(String::as_str) == Some("signal") {
        std::thread::sleep(delay(2));
        let sig = match args.get(1).map(String::as_str) {
            Some("SIGKILL") => perch::sys::sig::KILL,
            Some("SIGTERM") => perch::sys::sig::TERM,
            Some("SIGINT") => perch::sys::sig::INT,
            Some("SIGHUP") => perch::sys::sig::HUP,
            other => panic!("unknown signal {other:?}"),
        };
        perch::sys::reset_signals();
        let _ = perch::sys::kill(perch::sys::getpid(), sig);
        std::thread::sleep(Duration::from_secs(10));
        std::process::exit(99);
    }
    let code: i32 = args
        .first()
        .and_then(|c| c.parse().ok())
        .expect("usage: exit-with CODE [DELAY_MS]");
    std::thread::sleep(delay(1));
    std::process::exit(code);
}
