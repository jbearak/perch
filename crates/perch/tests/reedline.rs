// SPDX-License-Identifier: GPL-3.0-only

//! Raven's console at a prompt while detached: `reedline-prompt` (reedline
//! 0.52.1, which asks for the cursor position at every prompt and waits up
//! to 2 seconds for it) takes 200 sends and one 900 KB send with no prompt
//! waiting for an answer. After a model restart the model is partial and
//! the design's documented stall follows: each prompt waits the 2 seconds.
//! It ends when the program sends RIS.

#![cfg(feature = "test-hooks")]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::session::{SECS, control, info, request, send, send_ok};
use common::{Rt, testprog, wait_for};
use serde_json::{Value, json};

/// Longer than any answered prompt takes, shorter than crossterm's wait.
const NO_STALL_MS: u64 = 1500;

fn records(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    // Only whole lines: the program may be part way through writing one.
    let done = text.rfind('\n').map_or("", |i| &text[..i]);
    done.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn wait_records(path: &Path, n: usize, timeout: Duration) -> Vec<Value> {
    wait_for(timeout, || {
        let r = records(path);
        (r.len() >= n).then_some(r)
    })
    .unwrap_or_else(|| {
        let r = records(path);
        panic!(
            "{n} lines never arrived; got {}: {:?}",
            r.len(),
            r.iter()
                .filter(|x| !x["head"].as_str().is_some_and(|h| h.len() <= 8))
                .take(5)
                .collect::<Vec<_>>()
        )
    })
}

fn wait_model(rt: &Rt, id: &str, state: &str) {
    wait_for(SECS, || (info(rt, id)["model"] == state).then_some(()))
        .unwrap_or_else(|| panic!("the model never became {state}"));
}

/// Sends one line and waits for the program to read it. Returns how long
/// that took, which includes the prompt drawn before it was read.
fn line(c: &mut perch::client::Client, path: &Path, n: usize, text: &str) -> u64 {
    let start = Instant::now();
    c.request("send", json!({ "data": text, "submit": true }), SECS)
        .unwrap();
    let r = wait_records(path, n + 1, Duration::from_secs(30));
    assert_eq!(r[n]["head"], text, "{}", r[n]);
    start.elapsed().as_millis() as u64
}

#[test]
fn reedline_detached_never_waits_until_a_model_restart_and_again_after_ris() {
    let rt = Rt::new();
    let prog = testprog("reedline-prompt");
    let id = rt.new_session(&["--", prog.to_str().unwrap(), "{dir}/rl.out"]);
    let path = rt.dir(&id).join("rl.out");
    // Its first prompt: the terminal is in raw mode from here on. Lines
    // typed before that meet the kernel's canonical-mode limit, which
    // would cut them as in any terminal.
    wait_for(SECS, || info(&rt, &id)["lastOutputAt"].as_str().map(drop)).expect("the first prompt");

    // 200 sends, each once the line before it was read: reedline drops
    // lines typed ahead in the same read as an Enter, so a burst would
    // test reedline, not Perch.
    let mut c = control(&rt, &id);
    let mut n = 0;
    let mut worst = 0;
    for i in 0..200 {
        worst = worst.max(line(&mut c, &path, n, &format!("line-{i}")));
        n += 1;
    }
    assert!(worst < NO_STALL_MS, "a prompt waited {worst} ms");

    // One send of 900 KB, pasted.
    let big: Vec<u8> = b"abcdefghijklmnopqrstuvwxyz"
        .iter()
        .copied()
        .cycle()
        .take(900_000)
        .collect();
    let out = send(&rt, &id, &["--stdin", "--paste", "--submit"], Some(&big));
    assert!(out.status.success(), "{}", common::describe(&out));
    let r = wait_records(&path, n + 1, Duration::from_secs(120));
    assert_eq!(r[n]["len"], 900_000, "the paste is one line");
    n += 1;
    let ms = line(&mut c, &path, n, "after-big");
    n += 1;
    assert!(
        ms < NO_STALL_MS,
        "the prompt after the paste waited {ms} ms"
    );

    // The model restarts and is partial: each prompt waits for its own
    // timeout, as the design documents.
    request(&rt, &id, "test-model", json!({ "fault": "kill" }));
    wait_model(&rt, &id, "failed");
    line(&mut c, &path, n, "trigger");
    n += 1;
    wait_model(&rt, &id, "partial");
    let ms = line(&mut c, &path, n, "slow");
    n += 1;
    assert!(ms >= NO_STALL_MS, "the prompt did not wait: {ms} ms");

    // RIS completes the model, and prompts are answered again.
    line(&mut c, &path, n, "ris");
    n += 1;
    wait_model(&rt, &id, "ok");
    let mut worst = 0;
    for i in 0..5 {
        worst = worst.max(line(&mut c, &path, n, &format!("fast-{i}")));
        n += 1;
    }
    assert!(worst < NO_STALL_MS, "after RIS a prompt waited {worst} ms");
    assert!(
        records(&path).iter().all(|x| x.get("error").is_none()),
        "read_line never failed"
    );
    // The sends through the CLI work the same way.
    send_ok(&rt, &id, &["--text", "exit", "--submit"]);
}
