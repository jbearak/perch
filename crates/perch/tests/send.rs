// SPDX-License-Identifier: GPL-3.0-only

//! `send` end to end: the bytes a program reads after `perch send`, with
//! bracketed paste on, off, saved and restored; `submit`; `queue_full`;
//! no interleaving with typed input; `outcome_unknown`; and replies to
//! terminal queries ahead of queued sends, held back with the input gate.

#![cfg(feature = "test-hooks")]

mod common;

use std::time::Duration;

use common::session::{Timer, info, input_hook, send, send_env, send_ok, wait_input};
use common::term::{Recorder, contains};
use common::{Rt, testprog, wait_for};
use serde_json::{Value, json};

const SECS: Duration = Duration::from_secs(15);

/// What the round's last `drain` read.
fn drained(steps: &[Value]) -> String {
    steps
        .iter()
        .rev()
        .find_map(|s| s["read"].as_str())
        .expect("a drain step")
        .to_owned()
}

/// Everything the program read after sending the round's query: the
/// reply, what came with it, and what the drain step read.
fn after_query(steps: &[Value]) -> String {
    format!(
        "{}{}",
        steps[0]["reply"].as_str().expect("a reply"),
        drained(steps)
    )
}

/// What `perch send` with `args` makes the program read.
fn sent_as(rt: &Rt, id: &str, t: &mut Timer, args: &[&str]) -> String {
    send_ok(rt, id, args);
    drained(&t.round("drain:400"))
}

#[test]
fn paste_follows_bracketed_paste_and_submit_adds_a_return() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 2000);
    // Off at the start: raw.
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p1", "--paste"]),
        "p1"
    );
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "hi", "--submit"]),
        "hi\r"
    );
    assert_eq!(sent_as(&rt, &id, &mut t, &["--submit"]), "\r");
    t.round("paste:on");
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p2", "--paste", "--submit"]),
        "\x1b[200~p2\x1b[201~\r"
    );
    // Without --paste the bytes go raw even with bracketed paste on.
    assert_eq!(sent_as(&rt, &id, &mut t, &["--text", "p3"]), "p3");
    // Saved on, turned off, restored on.
    t.round("paste:save paste:off");
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p4", "--paste"]),
        "p4"
    );
    t.round("paste:restore");
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p5", "--paste"]),
        "\x1b[200~p5\x1b[201~"
    );
    // Saved off, turned on, restored off.
    t.round("paste:off paste:save paste:on paste:restore");
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p6", "--paste"]),
        "p6"
    );
    // RIS turns it off.
    t.round("paste:on ris");
    assert_eq!(
        sent_as(&rt, &id, &mut t, &["--text", "p7", "--paste"]),
        "p7"
    );
    // Bytes that are not UTF-8 go as base64 and arrive unchanged.
    let out = send(&rt, &id, &["--stdin"], Some(b"a\xffb"));
    assert!(out.status.success(), "{}", common::describe(&out));
    assert_eq!(drained(&t.round("drain:400")), "a\u{fffd}b");
    // A send is input: `lastInputAt` follows it.
    assert!(info(&rt, &id)["lastInputAt"].is_string());
}

/// `stdin-recorder` in a detached session, and a client attached to it
/// that types.
fn recorder_session(rt: &Rt) -> (String, std::path::PathBuf, Recorder) {
    let rec = rt.path.with_extension("rec");
    let _ = std::fs::remove_file(&rec);
    let prog = testprog("stdin-recorder");
    let id = rt.new_session(&["--", prog.to_str().unwrap(), rec.to_str().unwrap()]);
    let (r, answer) = Recorder::attach(rt, &id, json!({"restore": "none"}));
    assert_eq!(answer.kind, perch::proto::kind::ATTACHED);
    (id, rec, r)
}

fn wait_recorded(rec: &std::path::Path, want: &[u8]) {
    let ok = wait_for(SECS, || {
        (std::fs::read(rec).unwrap_or_default() == want).then_some(())
    });
    assert!(
        ok.is_some(),
        "recorded {} bytes, wanted {}: {:?}",
        std::fs::read(rec).unwrap_or_default().len(),
        want.len(),
        String::from_utf8_lossy(&std::fs::read(rec).unwrap_or_default())
            .chars()
            .take(200)
            .collect::<String>()
    );
}

#[test]
fn sends_never_interleave_with_typed_input_and_queue_full_is_atomic() {
    let rt = Rt::new();
    let (id, rec, mut r) = recorder_session(&rt);
    let mut want = Vec::new();
    // Typed, sent, typed: in that order, each whole.
    input_hook(&rt, &id, Some(Some(0)));
    r.input(b"ab");
    wait_input(&rt, &id, "the typed bytes", |v| v["queued"] == 2);
    send_ok(&rt, &id, &["--text", "SEND1"]);
    r.input(b"cd");
    wait_input(&rt, &id, "everything", |v| v["queued"] == 9);
    input_hook(&rt, &id, Some(None));
    want.extend_from_slice(b"abSEND1cd");
    wait_recorded(&rec, &want);

    // Keys typed while a send is partly written follow all of it.
    input_hook(&rt, &id, Some(Some(3)));
    send_ok(&rt, &id, &["--text", "LONGSEND-0123456789"]);
    wait_input(&rt, &id, "a partly written send", |v| v["writing"] == true);
    r.input(b"zz");
    wait_input(&rt, &id, "the keys", |v| v["queued"] == 16 + 2);
    input_hook(&rt, &id, Some(None));
    want.extend_from_slice(b"LONGSEND-0123456789zz");
    wait_recorded(&rec, &want);

    // A send that does not fit is refused whole, and keys still fit.
    input_hook(&rt, &id, Some(Some(0)));
    let big = vec![b'x'; 900_000];
    let out = send(&rt, &id, &["--stdin"], Some(&big));
    assert!(out.status.success(), "{}", common::describe(&out));
    let out = send(&rt, &id, &["--stdin"], Some(&vec![b'y'; 200_000]));
    assert_eq!(out.status.code(), Some(3), "{}", common::describe(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("full; nothing was sent"));
    r.input(b"k");
    wait_input(&rt, &id, "the key", |v| v["queued"] == 900_001);
    input_hook(&rt, &id, Some(None));
    want.extend_from_slice(&big);
    want.push(b'k');
    wait_recorded(&rec, &want);
    let _ = std::fs::remove_file(&rec);
}

#[test]
fn a_dropped_connection_before_queued_is_reported_and_never_resent() {
    let rt = Rt::new();
    let (id, rec, _r) = recorder_session(&rt);
    // The supervisor queues the send and drops the connection before its
    // answer, as if it had gone away at that moment.
    let out = send_env(
        &rt,
        &id,
        &["--text", "once"],
        None,
        &[("PERCH_TEST_SEND_DROP", "1")],
    );
    assert_eq!(out.status.code(), Some(4), "{}", common::describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("may not have been sent"),
        "{}",
        common::describe(&out)
    );
    wait_recorded(&rec, b"once");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(std::fs::read(&rec).unwrap(), b"once", "never resent");
    // A session that has gone: nothing was sent, and it says so.
    rt.ok(&["kill", &id]);
    let out = send(&rt, &id, &["--text", "late"], None);
    assert_eq!(out.status.code(), Some(1), "{}", common::describe(&out));
    let _ = std::fs::remove_file(&rec);
}

#[test]
fn a_supervisor_reply_goes_ahead_of_a_queued_send() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 5000);
    input_hook(&rt, &id, Some(Some(0)));
    send_ok(&rt, &id, &["--text", "AFTER"]);
    t.start("dsr5 drain:400");
    wait_input(&rt, &id, "the reply", |v| v["replies"] == 1);
    input_hook(&rt, &id, Some(None));
    let steps = t.wait();
    assert_eq!(steps[0]["stale"], "");
    // The program may read both at once; the reply comes first.
    assert_eq!(after_query(&steps), "\x1b[0nAFTER");
}

#[test]
fn a_reply_never_splits_a_large_send_partly_written() {
    let rt = Rt::new();
    let (id, mut t) = Timer::new_session(&rt, &[], 5000);
    let n = 300_000;
    input_hook(&rt, &id, Some(Some(0)));
    let out = send(&rt, &id, &["--stdin"], Some(&vec![b'x'; n]));
    assert!(out.status.success());
    // A thousand bytes reach the program before it asks.
    input_hook(&rt, &id, Some(Some(1000)));
    wait_input(&rt, &id, "a partly written send", |v| {
        v["writing"] == true && v["gate"] == 0
    });
    t.start("dsr6 drain:400");
    wait_input(&rt, &id, "the reply", |v| v["replies"] == 1);
    input_hook(&rt, &id, Some(None));
    let steps = t.wait();
    let stale = steps[0]["stale"].as_str().unwrap();
    let reply = steps[0]["reply"].as_str().expect("a reply");
    let at = reply.find('\x1b').expect("the cursor report");
    assert!(
        reply[at..].starts_with("\x1b[") && reply.ends_with('R'),
        "{reply:?}"
    );
    // The whole send, then the reply.
    assert!(stale.bytes().all(|b| b == b'x'));
    assert!(reply[..at].bytes().all(|b| b == b'x'));
    assert_eq!(stale.len() + at, n);
    assert_eq!(drained(&steps), "");
}

#[test]
fn a_client_reply_split_across_reads_goes_ahead_of_a_queued_send() {
    // Timers ten times as long: the reply hold becomes a second, far more
    // than the test needs between the two halves.
    let rt = Rt::with_scale("10");
    let (id, mut t) = Timer::new_session(&rt, &[], 5000);
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "none"}));
    input_hook(&rt, &id, Some(Some(0)));
    send_ok(&rt, &id, &["--text", "QUEUED"]);
    t.start("dsr6 drain:400");
    r.until(SECS, |r| contains(&r.output, b"\x1b[6n"));
    r.input(b"\x1b[12;");
    wait_input(&rt, &id, "the first half held", |v| v["holding"] == true);
    r.input(b"5R");
    wait_input(&rt, &id, "the reply", |v| v["replies"] == 1);
    input_hook(&rt, &id, Some(None));
    let steps = t.wait();
    assert_eq!(steps[0]["stale"], "");
    assert_eq!(after_query(&steps), "\x1b[12;5RQUEUED");
}
