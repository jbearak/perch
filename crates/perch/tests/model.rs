// SPDX-License-Identifier: GPL-3.0-only

//! The model process with real processes: detached query replies, the
//! model's descriptors, failures injected through test hooks and what
//! follows them (the program runs on, the model goes `failed` then
//! `partial`, restarts are limited), `info`'s live fields and `tail.txt`.

#![cfg(feature = "test-hooks")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Rt, alive, testprog, wait_for};
use perch::client::Client;
use serde_json::{Value, json};

fn s(p: &Path) -> &str {
    p.to_str().expect("UTF-8 path")
}

/// A control request to a session's supervisor.
fn control(rt: &Rt, id: &str, op: &str, args: Value) -> Value {
    let mut c =
        Client::connect(&rt.dir(id).join("sock"), "test", Duration::from_secs(5)).expect("connect");
    c.request(op, args, Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("{op}: {e}"))
}

/// The model's process, generation, state and OSC 7, after staging
/// `fault` if given.
fn test_model(rt: &Rt, id: &str, fault: Option<&str>) -> Value {
    let args = fault.map_or_else(|| json!({}), |f| json!({ "fault": f }));
    control(rt, id, "test-model", args)
}

fn info(rt: &Rt, id: &str) -> Value {
    serde_json::from_str(&rt.ok(&["info", id, "--json"])).expect("info JSON")
}

fn log(rt: &Rt, id: &str) -> String {
    std::fs::read_to_string(rt.dir(id).join("supervisor.log")).unwrap_or_default()
}

/// Waits for `info.model` to be `state`.
fn wait_model(rt: &Rt, id: &str, state: &str) {
    let seen = wait_for(Duration::from_secs(30), || {
        (info(rt, id)["model"] == state).then_some(())
    });
    assert!(
        seen.is_some(),
        "model never became {state}: {}\nlog:\n{}",
        info(rt, id),
        log(rt, id)
    );
}

/// Waits for the supervisor's log to contain `text`.
fn wait_log(rt: &Rt, id: &str, text: &str) {
    let seen = wait_for(Duration::from_secs(30), || {
        log(rt, id).contains(text).then_some(())
    });
    assert!(
        seen.is_some(),
        "the log never said {text:?}:\n{}",
        log(rt, id)
    );
}

/// A session running `dsr-timer`, driven a round at a time.
struct Timer {
    id: String,
    out: PathBuf,
    go: PathBuf,
    rounds: usize,
}

impl Timer {
    fn start(rt: &Rt, env: &[(&str, &str)]) -> Timer {
        let prog = testprog("dsr-timer");
        let mut cmd = rt.cmd();
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd
            .args(["new", "-d", "--", s(&prog), "{dir}/out.json", "{dir}/go"])
            .output()
            .expect("run perch new");
        assert!(out.status.success(), "{}", common::describe(&out));
        let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        Timer {
            out: rt.dir(&id).join("out.json"),
            go: rt.dir(&id).join("go"),
            id,
            rounds: 0,
        }
    }

    /// Runs one round of steps and returns its results.
    fn round(&mut self, steps: &str) -> Vec<Value> {
        let tmp = self.go.with_extension("tmp");
        std::fs::write(&tmp, steps).unwrap();
        std::fs::rename(&tmp, &self.go).unwrap();
        self.rounds += 1;
        let want = self.rounds;
        let line = wait_for(Duration::from_secs(60), || {
            let text = std::fs::read_to_string(&self.out).ok()?;
            text.lines().nth(want - 1).map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("round {want} ({steps}) never finished"));
        serde_json::from_str::<Vec<Value>>(&line).expect("round JSON")
    }

    /// Starts a round that does not finish (it ends the program).
    fn send(&self, steps: &str) {
        let tmp = self.go.with_extension("tmp");
        std::fs::write(&tmp, steps).unwrap();
        std::fs::rename(&tmp, &self.go).unwrap();
    }
}

fn reply(r: &Value) -> Option<&str> {
    r["reply"].as_str()
}

#[test]
fn detached_queries_are_answered_within_50_ms() {
    let rt = Rt::new();
    let mut t = Timer::start(&rt, &[]);
    let results = t.round("text:hello dsr6 dsr5 da1 da2 text:ab dsr6");
    let replies: Vec<Option<&str>> = results.iter().map(reply).collect();
    assert_eq!(
        replies,
        [
            None,
            Some("\x1b[2;1R"),
            Some("\x1b[0n"),
            Some("\x1b[?1;2c"),
            Some("\x1b[>0;0;0c"),
            None,
            Some("\x1b[3;1R"),
        ]
    );
    let mut times = Vec::new();
    for _ in 0..5 {
        let r = t.round("dsr6 dsr6 dsr5 da1");
        assert!(r.iter().all(|x| x["reply"].is_string()), "{r:?}");
        times.extend(r.iter().map(|x| x["ms"].as_f64().unwrap()));
    }
    let worst = times.iter().copied().fold(0.0, f64::max);
    assert!(worst < 50.0, "replies took up to {worst} ms: {times:?}");
    assert_eq!(info(&rt, &t.id)["model"], "ok");
}

#[test]
fn the_model_holds_only_its_socket_and_stderr() {
    let rt = Rt::new();
    let fds = rt.path.join("model-fds.txt");
    let t = Timer::start(&rt, &[("PERCH_TEST_MODEL_FDS", s(&fds))]);
    let text = rt.wait_file(&fds);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], "2 file", "stderr, on supervisor.log: {text}");
    assert!(lines[1].ends_with(" socket"), "{text}");
    // The program's own descriptors are unaffected.
    assert_eq!(info(&rt, &t.id)["model"], "ok");
}

/// How a failure shows before the model is restarted.
#[derive(PartialEq)]
enum Shows {
    /// `info.model` is `failed` until more output arrives.
    Failed,
    /// The output that made the model fail goes on, so a fresh model
    /// starts at once; the log shows the failure.
    LogOnly,
}

/// Runs `inject` against a working model and checks what the design
/// requires of every model failure: the program keeps running and its
/// output keeps flowing, `info.model` goes `failed`, then `partial` once
/// output arrives at a ground point, the partial model answers no DSR 6
/// until RIS, and the log says why it failed.
fn check_failure(
    env: &[(&str, &str)],
    inject: impl FnOnce(&Rt, &mut Timer),
    why: &[&str],
    shows: Shows,
) {
    let rt = Rt::new();
    let mut t = Timer::start(&rt, env);
    let r = t.round("dsr6");
    assert_eq!(reply(&r[0]), Some("\x1b[1;1R"));
    let before = test_model(&rt, &t.id, None);
    assert_eq!(before["generation"], 1);

    inject(&rt, &mut t);
    if shows == Shows::Failed {
        wait_model(&rt, &t.id, "failed");
    }
    wait_log(&rt, &t.id, "(generation 1) failed: ");
    let i = info(&rt, &t.id);
    assert_eq!(i["state"], "running");
    let pid = i["pid"].as_i64().unwrap() as i32;
    let start = rt.meta(&t.id)["pidStart"].as_str().unwrap().to_owned();
    assert!(alive(pid, &start), "the program died with the model");
    for text in why {
        wait_log(&rt, &t.id, text);
    }

    // Output at ground starts a fresh model, partial: DSR 5 is answered,
    // DSR 6 is not.
    let r = t.round("dsr5 dsr6");
    wait_log(&rt, &t.id, "started (generation 2, partial)");
    assert_eq!(reply(&r[0]), Some("\x1b[0n"));
    assert_eq!(reply(&r[1]), None, "a partial model answers no DSR 6");
    wait_model(&rt, &t.id, "partial");
    let after = test_model(&rt, &t.id, None);
    assert_eq!(after["generation"], 2);
    assert_ne!(after["pid"], before["pid"]);

    // RIS makes it complete.
    let r = t.round("ris text:x dsr6");
    assert_eq!(reply(&r[2]), Some("\x1b[2;1R"));
    assert_eq!(info(&rt, &t.id)["model"], "ok");
    assert!(alive(pid, &start));
}

/// A CSI that grows past 16 MiB is fed through, so the same model stays
/// and goes `partial` until RIS.
#[test]
fn an_oversized_csi_leaves_the_model_partial_until_ris() {
    let rt = Rt::new();
    let mut t = Timer::start(&rt, &[]);
    let r = t.round("dsr6");
    assert_eq!(reply(&r[0]), Some("\x1b[1;1R"));

    t.round("longcsi:17000000");
    wait_model(&rt, &t.id, "partial");

    let r = t.round("ris text:x dsr6");
    assert_eq!(reply(&r[2]), Some("\x1b[2;1R"));
    assert_eq!(info(&rt, &t.id)["model"], "ok");
    assert_eq!(test_model(&rt, &t.id, None)["generation"], 1);
    assert!(!log(&rt, &t.id).contains("failed"), "{}", log(&rt, &t.id));
}

#[test]
fn a_killed_model_is_replaced() {
    check_failure(
        &[],
        |rt, t| {
            test_model(rt, &t.id, Some("kill"));
        },
        &["killed by signal 9"],
        Shows::Failed,
    );
}

#[test]
fn a_hung_model_is_replaced_once_16_mib_behind() {
    check_failure(
        &[],
        |rt, t| {
            test_model(rt, &t.id, Some("hang"));
            // The program's output keeps flowing past the hung model.
            let r = t.round("flood:24000000 dsr5");
            assert_eq!(reply(&r[1]), Some("\x1b[0n"));
        },
        &["fell more than 16 MiB behind"],
        Shows::LogOnly,
    );
}

#[test]
fn repeated_long_sequences_do_not_hide_a_hung_model() {
    check_failure(
        &[],
        |rt, t| {
            test_model(rt, &t.id, Some("hang"));
            t.round("longcsi:17000000 ris longcsi:17000000 ris dsr5");
        },
        &["fell more than 16 MiB behind"],
        Shows::LogOnly,
    );
}

#[test]
fn a_model_over_its_memory_limit_is_replaced() {
    // Linux enforces RLIMIT_AS (1 GiB) in the model, so allocating past it
    // costs only address space. macOS has the supervisor read the resident
    // size; a lower limit keeps that cheap.
    let env: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[("PERCH_TEST_MODEL_MEMORY_LIMIT", "268435456")]
    } else {
        &[]
    };
    let why: &[&str] = if cfg!(target_os = "macos") {
        &["resident memory"]
    } else {
        &["memory allocation of", "SIGABRT"]
    };
    check_failure(
        env,
        |rt, t| {
            test_model(rt, &t.id, Some("grow"));
        },
        why,
        Shows::Failed,
    );
}

#[test]
fn three_missed_dsr6_deadlines_fail_the_model() {
    check_failure(
        &[],
        |rt, t| {
            test_model(rt, &t.id, Some("drop-replies"));
            let r = t.round("dsr6 dsr6 dsr6");
            assert!(r.iter().all(|x| x["reply"].is_null()), "{r:?}");
        },
        &["missed 3 DSR 6 deadlines within a minute"],
        Shows::Failed,
    );
}

#[test]
fn a_zig_abort_leaves_its_message_in_the_log() {
    check_failure(
        &[],
        |rt, t| {
            test_model(rt, &t.id, Some("abort"));
        },
        &["perch-vt test abort", "SIGABRT"],
        Shows::Failed,
    );
}

#[test]
fn a_reply_from_an_older_generation_is_discarded() {
    let rt = Rt::new();
    let mut t = Timer::start(&rt, &[]);
    test_model(&rt, &t.id, Some("stale-replies"));
    // The model sends a wrong reply (999;999) tagged with generation 0
    // before each real one.
    let r = t.round("text:a dsr6 dsr6");
    assert_eq!(reply(&r[1]), Some("\x1b[2;1R"));
    assert_eq!(reply(&r[2]), Some("\x1b[2;1R"));
    wait_log(
        &rt,
        &t.id,
        "discarded a message from model generation 0 (current 1)",
    );
    assert_eq!(info(&rt, &t.id)["model"], "ok");
}

#[test]
fn restarts_are_limited_to_three_an_hour() {
    let rt = Rt::new();
    let mut t = Timer::start(&rt, &[]);
    for generation in 2..=4 {
        test_model(&rt, &t.id, Some("kill"));
        wait_model(&rt, &t.id, "failed");
        t.round("dsr5");
        wait_model(&rt, &t.id, "partial");
        assert_eq!(test_model(&rt, &t.id, None)["generation"], generation);
    }
    test_model(&rt, &t.id, Some("kill"));
    wait_model(&rt, &t.id, "failed");
    let r = t.round("dsr5 dsr6");
    assert_eq!(reply(&r[0]), Some("\x1b[0n"), "passthrough goes on");
    assert_eq!(reply(&r[1]), None);
    wait_log(&rt, &t.id, "not restarting it for now");
    std::thread::sleep(Duration::from_millis(300));
    t.round("text:more");
    let m = test_model(&rt, &t.id, None);
    assert_eq!(m["state"], "failed");
    assert_eq!(m["generation"], 4);
    assert_eq!(info(&rt, &t.id)["state"], "running");
}

#[test]
fn info_reports_model_memory_directory_and_foreground() {
    let rt = Rt::new();
    let dir = rt.path.join("work dir");
    std::fs::create_dir(&dir).unwrap();
    let script = format!(
        "cd '{}' && printf '\\033]7;file://host/osc%%207/dir\\007' && exec sleep 1000",
        s(&dir)
    );
    let id = rt.new_session(&["--", "sh", "-c", &script]);
    let pwd = wait_for(Duration::from_secs(10), || {
        test_model(&rt, &id, None)["pwd"]
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(pwd.as_deref(), Some("file://host/osc%207/dir"));
    let i = wait_for(Duration::from_secs(10), || {
        let i = info(&rt, &id);
        (i["foreground"]["name"] == "sleep").then_some(i)
    })
    .expect("sleep in the foreground");
    assert_eq!(i["model"], "ok");
    assert!(i["memoryBytes"].as_u64().unwrap() > 100_000, "{i}");
    assert_eq!(i["foreground"]["pid"], i["pid"]);
    // The live directory wins over OSC 7 and the start directory.
    assert_eq!(
        Path::new(i["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        dir.canonicalize().unwrap()
    );
    let listed = rt.listed(&id).unwrap();
    assert_eq!(listed["cwd"], i["cwd"]);
    assert_eq!(listed["model"], "ok");
    let text = rt.ok(&["info", &id]);
    for want in ["memory:", "about", "foreground:", "sleep (", "model:"] {
        assert!(text.contains(want), "{text}");
    }
}

/// Waits for a session to end and returns its `tail.txt`.
fn tail_after_exit(rt: &Rt, id: &str) -> String {
    wait_for(Duration::from_secs(30), || {
        (rt.meta(id)["state"] == "exited").then_some(())
    })
    .unwrap_or_else(|| panic!("never exited; log:\n{}", log(rt, id)));
    std::fs::read_to_string(rt.dir(id).join("tail.txt")).expect("tail.txt")
}

#[test]
fn tail_txt_holds_the_last_screen_and_history() {
    let rt = Rt::new();
    let script = "i=0; while [ $i -lt 60 ]; do echo line $i; i=$((i+1)); done; \
                  printf 'last \\033[31mred\\033[0m\\n'; exit 3";
    let id = rt.new_session(&["--", "sh", "-c", script]);
    let tail = tail_after_exit(&rt, &id);
    assert_eq!(rt.meta(&id)["exit"], json!({ "code": 3 }));
    // History that scrolled off the 24-row screen is there, in order.
    let at = |needle: &str| {
        tail.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from {tail:?}"))
    };
    assert!(at("line 0\n") < at("line 30\n") && at("line 30\n") < at("line 59\n"));
    assert!(at("last ") < at("red"));
    assert!(tail.contains("\x1b["), "SGR kept: {tail:?}");
    assert!(tail.contains("31"), "the red: {tail:?}");
}

#[test]
fn tail_txt_is_a_note_when_the_model_is_partial_or_failed() {
    let rt = Rt::new();
    // Partial: restarted after a kill.
    let mut t = Timer::start(&rt, &[]);
    test_model(&rt, &t.id, Some("kill"));
    wait_model(&rt, &t.id, "failed");
    t.round("text:after");
    wait_model(&rt, &t.id, "partial");
    t.send("text:bye exit:0");
    let tail = tail_after_exit(&rt, &t.id);
    assert!(
        tail.starts_with("[perch] The last screen was not recorded") && tail.contains("restarted"),
        "{tail:?}"
    );
    assert!(!tail.contains("bye"));

    // Failed: a hung model misses the 2-second snapshot deadline.
    let mut t = Timer::start(&rt, &[]);
    t.round("text:before");
    test_model(&rt, &t.id, Some("hang"));
    t.send("exit:0");
    let tail = tail_after_exit(&rt, &t.id);
    assert!(tail.contains("the terminal model had failed"), "{tail:?}");
    assert!(log(&rt, &t.id).contains("missed its 2-second snapshot deadline"));
}
