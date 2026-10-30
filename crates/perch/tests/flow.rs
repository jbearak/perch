// SPDX-License-Identifier: GPL-3.0-only

//! Flow control and live queries end to end: a stopped client, a slow
//! client, keystrokes and resizes under backpressure, stalls in the middle
//! of a restore and of live output ending with a correct screen, `too_slow`,
//! and at most one reply to each query across attach, takeover, stall and
//! detach.
//!
//! The stall and too-slow rules run on real time (5 seconds), so these
//! tests take a while. The screens after a stall are recorded like
//! `attach_fidelity.rs`'s, to `$PERCH_E2E_RECORDINGS` if set, for
//! `node tests/fidelity/run.mjs --recorded`.

#![cfg(feature = "test-hooks")]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::session::{SECS, Timer, flood_bytes, info, input_hook, request};
use common::term::{Pty, Recorder, contains, find};
use common::{Rt, testprog, wait_for};
use perch::proto::kind;
use perch::restore;
use serde_json::{Value, json};

/// The attached client's flow, from the test hook.
fn flow(rt: &Rt, id: &str) -> Value {
    input_hook(rt, id, None)["client"].clone()
}

fn wait_phase(rt: &Rt, id: &str, phase: &str, timeout: Duration) {
    wait_for(timeout, || (flow(rt, id)["phase"] == phase).then_some(()))
        .unwrap_or_else(|| panic!("never {phase}: {}", flow(rt, id)));
}

/// How many restores (prologues) `out` holds.
fn restores(out: &[u8]) -> usize {
    let p = restore::prologue(80);
    out.windows(p.len()).filter(|w| *w == p.as_slice()).count()
}

/// Writes a recording for the fidelity harness: what the program wrote
/// before and after (`program`), what the client received up to that
/// point (`received`) and after (`live`).
fn record(name: &str, program: [&[u8]; 2], received: &[u8], live: &[u8]) {
    let Some(base) = std::env::var_os("PERCH_E2E_RECORDINGS").map(PathBuf::from) else {
        return;
    };
    let dir = base.join(format!("{name}-screen"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("program-0.bin"), program[0]).unwrap();
    std::fs::write(dir.join("program-1.bin"), program[1]).unwrap();
    std::fs::write(dir.join("restore.bin"), received).unwrap();
    std::fs::write(dir.join("live.bin"), live).unwrap();
    let meta = json!({
        "fixture": name,
        "mode": "screen",
        "size": [80, 24],
        "scrollback": 1000,
        "client_scrollback": 1000,
    });
    std::fs::write(dir.join("recording.json"), format!("{meta:#}\n")).unwrap();
}

#[test]
fn a_stopped_client_holds_the_program_at_most_5_seconds_then_resyncs() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 2000);
    t.round("text:start");
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    r.expect(b"start");
    r.settle(Duration::from_millis(200));
    // The client stops reading. The program writes far more than the
    // queue, the socket and the PTY hold.
    let n = 12_000_000;
    let steps = t.round(&format!("flood:{n} text:done"));
    let ms = steps[0]["ms"].as_f64().unwrap();
    assert!(
        (4500.0..12_000.0).contains(&ms),
        "the program was held {ms} ms"
    );
    wait_phase(&rt, &id, "Lagging", SECS);
    // Reading again, the client gets a resync with the screen as it is.
    r.expect(b"done");
    r.settle(Duration::from_millis(300));
    assert!(restores(&r.output) >= 2, "a resync after the stall");
    let received = std::mem::take(&mut r.output);
    t.round("text:after");
    r.expect(b"after\r\n");
    r.settle(Duration::from_millis(200));
    assert_eq!(r.output, b"after\r\n", "live again");
    let mut program = b"start\r\n".to_vec();
    program.extend(flood_bytes(n));
    program.extend_from_slice(b"done\r\n");
    record("stall-live", [&program, b"after\r\n"], &received, &r.output);
}

#[test]
fn a_stall_in_the_middle_of_a_restore_ends_in_a_resync() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &["--scrollback", "10000"], 2000);
    let n = 2_000_000;
    t.round(&format!("flood:{n} text:end-of-history"));
    // A full restore with 10,000 lines of history, far more than the
    // socket holds; the client reads none of it.
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "full", "scrollback": 10000}));
    wait_phase(&rt, &id, "Lagging", Duration::from_secs(20));
    // Output while it lags reaches the model only.
    t.round("text:after-stall");
    r.expect(b"after-stall");
    r.settle(Duration::from_millis(300));
    assert!(restores(&r.output) >= 2, "a resync after the stall");
    let received = std::mem::take(&mut r.output);
    t.round("text:live");
    r.expect(b"live\r\n");
    r.settle(Duration::from_millis(200));
    let mut program = flood_bytes(n);
    program.extend_from_slice(b"end-of-history\r\nafter-stall\r\n");
    record(
        "stall-restore",
        [&program, b"live\r\n"],
        &received,
        &r.output,
    );
}

#[test]
fn a_slow_client_slows_the_program_and_loses_nothing() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 2000);
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    r.settle(Duration::from_millis(300));
    let from = r.output.len();
    let n = 8_000_000;
    t.start(&format!("flood:{n}"));
    let end = Instant::now() + Duration::from_secs(90);
    let mut backpressure = false;
    let mut turns = 0;
    while r.output.len() - from < n {
        assert!(Instant::now() < end, "the flood never arrived");
        r.read_some(16 * 1024, Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(2));
        turns += 1;
        if !backpressure && turns % 25 == 0 {
            backpressure = flow(&rt, &id)["readBudget"] == 0;
        }
    }
    t.wait();
    r.settle(Duration::from_millis(200));
    assert!(backpressure, "the queue filled and the PTY waited");
    assert!(r.output[from..] == flood_bytes(n), "every byte, in order");
    assert_eq!(restores(&r.output), 1, "no resync");
}

#[test]
fn keystrokes_and_resizes_pass_under_backpressure() {
    let rt = Rt::new();
    let rec = rt.path.with_extension("rec");
    let _ = std::fs::remove_file(&rec);
    let prog = testprog("stdin-recorder");
    let id = rt.new_session(&[
        "--",
        prog.to_str().unwrap(),
        rec.to_str().unwrap(),
        "--flood",
    ]);
    let a = Pty::spawn(&rt, &["attach", &id], 80, 24);
    a.expect(0, b"flood line");
    // The terminal stops reading: `perch attach` blocks writing to it,
    // stops reading the socket, and the supervisor stops reading the PTY.
    a.pause(true);
    wait_for(SECS, || (flow(&rt, &id)["readBudget"] == 0).then_some(())).expect("backpressure");
    let mut a = a;
    a.type_bytes(b"abc\x03");
    wait_for(SECS, || {
        (std::fs::read(&rec).unwrap_or_default() == b"abc\x03").then_some(())
    })
    .expect("the keys reach the program");
    a.resize(100, 30);
    wait_for(SECS, || (info(&rt, &id)["cols"] == 100).then_some(()))
        .expect("the resize reaches the PTY");
    assert_eq!(info(&rt, &id)["rows"], 30);
    // Still held: this all happened under backpressure, not after a stall.
    assert_eq!(flow(&rt, &id)["phase"], "Live", "{}", flow(&rt, &id));
    assert_eq!(flow(&rt, &id)["readBudget"], 0);
    a.pause(false);
    let _ = std::fs::remove_file(&rec);
}

#[test]
fn three_stalls_in_ten_minutes_detach_the_client_as_too_slow() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 2000);
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    r.settle(Duration::from_millis(300));
    for stall in 1..=3 {
        // Not reading: the flood stalls the client after 5 seconds.
        t.round("flood:12000000");
        if stall == 3 {
            break;
        }
        wait_phase(&rt, &id, "Lagging", SECS);
        // Reading again until the resync is through and it is live.
        let end = Instant::now() + SECS;
        loop {
            r.read_some(64 * 1024, Duration::from_millis(50));
            let f = flow(&rt, &id);
            if f["phase"] == "Live" && f["liveBytes"] == 0 {
                break;
            }
            assert!(Instant::now() < end, "never live again: {f}");
        }
        r.settle(Duration::from_millis(200));
    }
    assert!(r.until(Duration::from_secs(30), |r| r.closed));
    let d = r.frame(kind::DETACHED).expect("a detached frame");
    assert_eq!(d["reason"], "too_slow");
    assert_eq!(info(&rt, &id)["attached"], false);
}

// ---------------------------------------------------------------------------
// At most one reply per query.

/// The reply the program read for the round's query and what it read
/// afterwards: a second reply would show up there.
fn reply_and_after(steps: &[Value]) -> (Value, String) {
    let after = steps
        .iter()
        .rev()
        .find_map(|s| s["read"].as_str())
        .unwrap_or("")
        .to_owned();
    (steps[0]["reply"].clone(), after)
}

/// Waits until the client has received `query` (after `from`).
fn sees(r: &mut Recorder, from: usize, query: &[u8]) {
    assert!(
        r.until(SECS, |r| contains(&r.output[from..], query)),
        "the client never got {query:?}"
    );
}

#[test]
fn a_query_the_client_answers_gets_only_its_answer() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 3000);
    let (mut a, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    a.settle(Duration::from_millis(200));
    let from = a.output.len();
    t.start("dsr6 drain:500");
    sees(&mut a, from, b"\x1b[6n");
    a.input(b"\x1b[5;5R");
    let (reply, after) = reply_and_after(&t.wait());
    assert_eq!(reply, "\x1b[5;5R");
    assert_eq!(after, "");
    // Answered, then gone at once: the supervisor does not answer too.
    let from = a.output.len();
    t.start("dsr5 drain:500");
    sees(&mut a, from, b"\x1b[5n");
    a.input(b"\x1b[0n");
    a.close();
    let (reply, after) = reply_and_after(&t.wait());
    assert_eq!(reply, "\x1b[0n");
    assert_eq!(after, "");
}

#[test]
fn a_takeover_with_a_query_outstanding_gets_one_reply() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 3000);
    let (mut a, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    a.settle(Duration::from_millis(200));
    let from = a.output.len();
    t.start("da1 drain:500");
    sees(&mut a, from, b"\x1b[c");
    // Another client takes over before the first answers: the supervisor
    // answers, and the first client's late answer goes nowhere.
    let (mut b, answer) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    assert_eq!(answer.kind, kind::ATTACHED);
    a.late_input(b"\x1b[?62;22c");
    let (reply, after) = reply_and_after(&t.wait());
    assert_eq!(reply, "\x1b[?1;2c");
    assert_eq!(after, "");
    b.settle(Duration::from_millis(200));
    assert!(
        !contains(&b.output, b"\x1b[c"),
        "the new client never saw it"
    );
}

#[test]
fn a_detach_with_a_query_outstanding_gets_one_reply() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 3000);
    // `perch detach`.
    let (mut a, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    a.settle(Duration::from_millis(200));
    let from = a.output.len();
    t.start("dsr5 drain:500");
    sees(&mut a, from, b"\x1b[5n");
    rt.ok(&["detach", &id]);
    a.late_input(b"\x1b[0n");
    let (reply, after) = reply_and_after(&t.wait());
    assert_eq!(reply, "\x1b[0n");
    assert_eq!(after, "");
    // The client closes its connection.
    let (mut b, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    b.settle(Duration::from_millis(200));
    let from = b.output.len();
    t.start("dsr6 drain:500");
    sees(&mut b, from, b"\x1b[6n");
    b.close();
    let (reply, after) = reply_and_after(&t.wait());
    let reply = reply.as_str().expect("the supervisor answers").to_owned();
    assert!(
        reply.starts_with("\x1b[") && reply.ends_with('R'),
        "{reply:?}"
    );
    assert_eq!(after, "");
}

#[test]
fn a_query_the_client_never_answers_expires_unanswered() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 3000);
    let (mut a, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    a.settle(Duration::from_millis(200));
    let from = a.output.len();
    t.start("dsr6");
    sees(&mut a, from, b"\x1b[6n");
    // Its 2 seconds pass; a detach afterwards answers nothing.
    let (reply, _) = reply_and_after(&t.wait());
    assert_eq!(reply, Value::Null);
    a.close();
    wait_for(SECS, || (info(&rt, &id)["attached"] == false).then_some(())).expect("detached");
    let (_, after) = reply_and_after(&t.round("drain:500"));
    assert_eq!(after, "");
}

#[test]
fn a_stall_with_a_query_in_dropped_output_gets_one_reply() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 3000);
    let (mut a, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    a.settle(Duration::from_millis(200));
    let from = a.output.len();
    // The client stops reading; the query comes after a flood that stalls
    // it, so it is in dropped output or read while the client lags.
    let steps = t.round("flood:12000000 dsr6 drain:500");
    let (reply, after) = reply_and_after(&steps[1..]);
    let reply = reply.as_str().expect("the supervisor answers").to_owned();
    assert!(
        reply.starts_with("\x1b[") && reply.ends_with('R'),
        "{reply:?}"
    );
    assert_eq!(after, "");
    // The client gets a resync, never the query itself, which its
    // terminal would answer a second time.
    let end = Instant::now() + SECS;
    while restores(&a.output[from..]) == 0 {
        assert!(Instant::now() < end, "no resync");
        a.read_some(64 * 1024, Duration::from_millis(50));
    }
    a.settle(Duration::from_millis(300));
    assert!(!contains(&a.output[from..], b"\x1b[6n"));
    let at = find(&a.output[from..], &restore::prologue(80)).unwrap();
    assert!(at > 0, "some live output came before the stall");
    let _ = request(&rt, &id, "info", json!({}));
}
