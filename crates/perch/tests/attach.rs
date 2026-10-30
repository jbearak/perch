// SPDX-License-Identifier: GPL-3.0-only

//! Attaching with real processes: `perch new` and `perch attach` on
//! terminals of the test's own, and a recording client for exact bytes.

#![cfg(feature = "test-hooks")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::term::{Pty, Recorder, contains, find};
use common::{Rt, testprog, wait_for};
use perch::proto::kind;
use perch::restore;
use serde_json::{Value, json};

const SECS: Duration = Duration::from_secs(15);

fn s(p: &Path) -> &str {
    p.to_str().expect("UTF-8 path")
}

fn info(rt: &Rt, id: &str) -> Value {
    serde_json::from_str(&rt.ok(&["info", id, "--json"])).expect("info JSON")
}

/// The only session in `rt`, once it is running.
fn only_session(rt: &Rt) -> String {
    wait_for(SECS, || {
        let v = rt.list(false);
        let s = v["sessions"].as_array()?.first()?.clone();
        (s["state"] == "running").then(|| s["id"].as_str().unwrap().to_owned())
    })
    .expect("a running session")
}

fn wait_attached(rt: &Rt, id: &str, attached: bool) {
    wait_for(SECS, || {
        (info(rt, id)["attached"] == attached).then_some(())
    })
    .unwrap_or_else(|| panic!("attached never became {attached}"));
}

/// Drives `dsr-timer` in a session: writes the words to its GO file and
/// waits for the round to finish.
struct Timer {
    out: std::path::PathBuf,
    go: std::path::PathBuf,
    rounds: usize,
}

impl Timer {
    fn args(rt: &Rt) -> Vec<String> {
        let _ = rt;
        vec![
            s(&testprog("dsr-timer")).to_owned(),
            "{dir}/timer.out".into(),
            "{dir}/timer.go".into(),
            "200".into(),
        ]
    }

    fn of(rt: &Rt, id: &str) -> Timer {
        Timer {
            out: rt.dir(id).join("timer.out"),
            go: rt.dir(id).join("timer.go"),
            rounds: 0,
        }
    }

    /// Starts a round without waiting for it: one that ends in `exit:`.
    fn send(&self, words: &str) {
        let tmp = self.go.with_extension("tmp");
        std::fs::write(&tmp, words).unwrap();
        std::fs::rename(&tmp, &self.go).unwrap();
    }

    fn round(&mut self, words: &str) {
        self.send(words);
        self.rounds += 1;
        let want = self.rounds;
        wait_for(Duration::from_secs(60), || {
            let text = std::fs::read_to_string(&self.out).ok()?;
            (text.lines().count() >= want).then_some(())
        })
        .unwrap_or_else(|| panic!("round `{words}` never finished"));
    }
}

fn new_timer_detached(rt: &Rt) -> (String, Timer) {
    let mut args = vec!["--"];
    let prog = Timer::args(rt);
    args.extend(prog.iter().map(String::as_str));
    let id = rt.new_session(&args);
    let t = Timer::of(rt, &id);
    (id, t)
}

#[test]
fn new_attach_detach_and_reattach() {
    let rt = Rt::new();
    let prog = Timer::args(&rt);
    let mut args = vec!["new", "--name", "t", "--"];
    args.extend(prog.iter().map(String::as_str));
    let mut a = Pty::spawn(&rt, &args, 80, 24);
    let id = only_session(&rt);
    let mut t = Timer::of(&rt, &id);
    // The program starts at the creator's attach, on its terminal.
    t.round("text:hello-live");
    a.expect(0, b"hello-live");
    let i = info(&rt, &id);
    assert_eq!(i["attached"], true);
    assert_eq!(i["clientPid"], a.pid());
    assert!(
        i["clientLabel"].as_str().unwrap().starts_with("tty:/dev/"),
        "{i}"
    );

    // Ctrl-\ detaches.
    a.type_bytes(b"\x1c");
    assert_eq!(a.exit_code(), 0);
    a.expect(0, b"[perch] Detached.");
    wait_attached(&rt, &id, false);

    // Output while detached appears on the next attach.
    t.round("text:while-away");
    let mut b = Pty::spawn(&rt, &["attach", &id], 80, 24);
    b.expect(0, b"while-away");
    wait_attached(&rt, &id, true);
    let from = b.output_len();
    t.round("text:live-again");
    b.expect(from, b"live-again");

    // `perch detach` from outside.
    rt.ok(&["detach", &id]);
    assert_eq!(b.exit_code(), 0);
    let out = rt.perch(&["detach", &id]);
    assert_eq!(out.status.code(), Some(1), "nothing attached");

    // A full restore brings the history back too.
    let (mut r, answer) = Recorder::attach(&rt, &id, json!({"restore": "full"}));
    assert_eq!(answer.kind, kind::ATTACHED);
    r.expect(b"live-again");
    for word in [&b"hello-live"[..], b"while-away"] {
        assert!(contains(&r.output, word));
    }
}

#[test]
fn full_and_screen_restores_end_to_end() {
    let rt = Rt::new();
    let (id, mut t) = new_timer_detached(&rt);
    t.round("flood:40000 text:the-end");
    for mode in ["screen", "full"] {
        let (mut r, answer) =
            Recorder::attach(&rt, &id, json!({"restore": mode, "rows": 10, "cols": 50}));
        assert_eq!(
            serde_json::from_slice::<Value>(&answer.payload).unwrap(),
            json!({"rows": 10, "cols": 50})
        );
        r.expect(b"the-end");
        r.settle(Duration::from_millis(300));
        assert!(r.output.starts_with(&restore::prologue(50)), "{mode}");
        assert_eq!(contains(&r.output, b"\x1b[3J"), mode == "full", "{mode}");
        // Full sends history: far more lines than fit the screen.
        let lines = r.output.windows(2).filter(|w| w == b"\r\n").count();
        if mode == "full" {
            assert!(lines > 100, "{mode}: {lines} lines");
        } else {
            assert!(lines < 20, "{mode}: {lines} lines");
        }
        let i = info(&rt, &id);
        assert_eq!(
            (i["rows"].as_u64(), i["cols"].as_u64()),
            (Some(10), Some(50))
        );
        // The client's resize reaches the PTY and the model.
        r.resize(60, 12);
        wait_for(SECS, || (info(&rt, &id)["cols"] == 60).then_some(())).expect("resized");
        assert_eq!(info(&rt, &id)["rows"], 12);
        r.close();
        wait_attached(&rt, &id, false);
    }
}

#[test]
fn takeover_and_taking_it_back() {
    let rt = Rt::new();
    let (id, mut t) = new_timer_detached(&rt);
    t.round("text:first");
    let mut a = Pty::spawn(&rt, &["attach", &id], 80, 24);
    a.expect(0, b"first");
    wait_attached(&rt, &id, true);
    let mut b = Pty::spawn(&rt, &["attach", &id, "--restore", "full"], 80, 24);
    b.expect(0, b"first");
    a.expect(0, b"Opened in another window. Press Enter to take it back");
    wait_for(SECS, || {
        (info(&rt, &id)["clientPid"] == b.pid()).then_some(())
    })
    .expect("b attached");
    // Live output goes to the new client only.
    let (fa, fb) = (a.output_len(), b.output_len());
    t.round("text:second");
    b.expect(fb, b"second");
    assert!(!contains(&a.output()[fa..], b"second"));
    // Enter takes it back, with a restore.
    a.type_bytes(b"\r");
    a.expect(fa, b"second");
    b.expect(fb, b"Opened in another window.");
    wait_for(SECS, || {
        (info(&rt, &id)["clientPid"] == a.pid()).then_some(())
    })
    .expect("a attached again");
    // Ctrl-C at the prompt leaves.
    b.type_bytes(b"\x03");
    assert_eq!(b.exit_code(), 0);
}

#[test]
fn if_detached_finds_a_live_client_and_exits_75() {
    let rt = Rt::new();
    let (id, _t) = new_timer_detached(&rt);
    let (_r, answer) = Recorder::attach(&rt, &id, json!({}));
    assert_eq!(answer.kind, kind::ATTACHED);
    let (_r2, answer) = Recorder::attach(&rt, &id, json!({"ifDetached": true}));
    assert_eq!(answer.kind, kind::BUSY);
    let out = rt.perch(&["attach", &id, "--if-detached"]);
    assert_eq!(out.status.code(), Some(75), "{}", common::describe(&out));
    // The first client is still the one attached.
    assert_eq!(info(&rt, &id)["clientLabel"], "test-recorder");
}

#[test]
fn a_creator_that_never_attaches_leaves_nothing() {
    let rt = Rt::new();
    let out = rt
        .cmd()
        .env("PERCH_TEST_NO_ATTACH", "1")
        .args(["new", "--", s(&testprog("proc-state")), "{dir}/state.json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::describe(&out));
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert_eq!(rt.meta(&id)["state"], "starting");
    // 30 seconds, scaled to 3.
    wait_for(Duration::from_secs(20), || {
        (!rt.dir(&id).exists()).then_some(())
    })
    .expect("the directory is removed");
    assert!(!rt.dir(&id).join("state.json").exists());
    assert_eq!(
        std::fs::read_dir(&rt.path).unwrap().count(),
        0,
        "nothing left behind"
    );

    // A kill while it waits ends it too.
    let out = rt
        .cmd()
        .env("PERCH_TEST_NO_ATTACH", "1")
        .args(["new", "--", "sleep", "1000"])
        .output()
        .unwrap();
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    rt.ok(&["kill", &id]);
    assert!(!rt.dir(&id).exists());
}

#[test]
fn exit_while_attached_gives_the_exit_code_and_removes_the_directory() {
    let rt = Rt::new();
    let prog = s(&testprog("exit-with")).to_owned();
    let mut a = Pty::spawn(&rt, &["new", "--", &prog, "7", "300"], 80, 24);
    let id = only_session(&rt);
    assert_eq!(a.exit_code(), 7);
    a.expect(0, b"[perch] The program ended: exit code 7.");
    assert!(!rt.dir(&id).exists());
    assert_eq!(std::fs::read_dir(&rt.path).unwrap().count(), 0);

    // Killed by a signal: 128 plus its number.
    let mut a = Pty::spawn(
        &rt,
        &["new", "--", &prog, "signal", "SIGKILL", "300"],
        80,
        24,
    );
    assert_eq!(a.exit_code(), 128 + 9);
    // A program that cannot start: the creator hears why.
    let mut a = Pty::spawn(&rt, &["new", "--", "/nonexistent/program"], 80, 24);
    assert_eq!(a.exit_code(), 127);
    a.expect(0, b"/nonexistent/program");
    assert_eq!(std::fs::read_dir(&rt.path).unwrap().count(), 0);
}

#[test]
fn output_written_before_the_exit_reaches_the_client() {
    let rt = Rt::new();
    let (id, t) = new_timer_detached(&rt);
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    r.settle(Duration::from_millis(200));
    t.send("flood:300000 text:last-words exit:3");
    assert!(r.until(SECS, |r| r.closed), "the connection closes");
    assert!(contains(&r.output, b"last-words"));
    let exited = r.frame(kind::EXITED).expect("an exited frame");
    assert_eq!(exited["status"], 3);
    assert_eq!(exited["exit"]["code"], 3);
    // Every byte of the flood arrived before it.
    let line = [b"0123456789abcdef".repeat(4).as_slice(), b"\r\n"].concat();
    let start = find(&r.output, &line).expect("the flood");
    // dsr-timer writes the same 64 KiB block over and over.
    let block: Vec<u8> = line.iter().copied().cycle().take(64 * 1024).collect();
    let flood: Vec<u8> = block.iter().copied().cycle().take(300_000).collect();
    let got = &r.output[start..];
    let same = got.iter().zip(&flood).take_while(|(a, b)| a == b).count();
    assert_eq!(
        same,
        flood.len(),
        "the flood differs at {same}: {:?}",
        String::from_utf8_lossy(&got[same.saturating_sub(20)..(same + 80).min(got.len())])
    );
    assert!(!rt.dir(&id).exists());
}

/// Where `stdin-recorder` records, outside the session directory so the
/// recording outlives the session.
fn rec_path(rt: &Rt) -> std::path::PathBuf {
    rt.path.with_extension("rec")
}

/// What `stdin-recorder` read so far.
fn recorded(rt: &Rt) -> Vec<u8> {
    std::fs::read(rec_path(rt)).unwrap_or_default()
}

fn wait_recorded(rt: &Rt, want: &[u8]) {
    let ok = wait_for(SECS, || (recorded(rt) == want).then_some(()));
    assert!(
        ok.is_some(),
        "recorded {:?}, typed {:?}",
        String::from_utf8_lossy(&recorded(rt)),
        String::from_utf8_lossy(want)
    );
}

/// Starts `stdin-recorder` with `perch new` on a terminal.
fn new_recorder(rt: &Rt, extra: &[&str]) -> (Pty, String) {
    let prog = s(&testprog("stdin-recorder")).to_owned();
    let rec = s(&rec_path(rt)).to_owned();
    let _ = std::fs::remove_file(&rec);
    let mut args = vec!["new"];
    args.extend_from_slice(extra);
    args.extend(["--", &prog, &rec]);
    let a = Pty::spawn(rt, &args, 80, 24);
    a.expect(0, b"recording");
    let id = only_session(rt);
    (a, id)
}

/// The invariant from the brief: Perch never writes into a session on its
/// own. Through attach, detach, takeover, every restore mode, resizes and
/// kill, the program reads exactly the bytes that were typed.
#[test]
fn no_injection() {
    let rt = Rt::new();
    let mut typed = Vec::new();
    let (mut a, id) = new_recorder(&rt, &["--restore", "full"]);
    let type_into = |p: &mut Pty, bytes: &[u8], typed: &mut Vec<u8>| {
        p.type_bytes(bytes);
        typed.extend_from_slice(bytes);
        wait_recorded(&rt, typed);
    };
    type_into(&mut a, b"first line\r", &mut typed);
    a.resize(100, 30);
    type_into(&mut a, b"\x1b[A\x1b[B up and down \x7f\t", &mut typed);
    a.resize(40, 10);
    // Detach: the key is not typed into the program.
    a.type_bytes(b"x\x1c");
    typed.push(b'x');
    assert_eq!(a.exit_code(), 0);
    wait_recorded(&rt, &typed);

    // Every restore mode.
    let mut b = Pty::spawn(&rt, &["attach", &id], 80, 24);
    b.expect(0, b"recording");
    wait_attached(&rt, &id, true);
    type_into(&mut b, "screen ünïcode\r".as_bytes(), &mut typed);
    b.resize(120, 40);
    // Takeover, with a full restore.
    let mut c = Pty::spawn(&rt, &["attach", &id, "--restore", "full"], 90, 20);
    b.expect(0, b"Opened in another window");
    c.expect(0, b"recording");
    // The old client's keys go nowhere now.
    b.type_bytes(b"ignored");
    type_into(&mut c, b"full\r", &mut typed);
    // A recording client with no restore, and a resize from it.
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "none"}));
    c.expect(0, b"Opened in another window");
    r.resize(70, 22);
    r.input(b"none\r");
    typed.extend_from_slice(b"none\r");
    wait_recorded(&rt, &typed);
    r.close();
    wait_attached(&rt, &id, false);
    // `perch detach` with a client attached, then the session is killed.
    let mut d = Pty::spawn(&rt, &["attach", &id], 80, 24);
    d.expect(0, b"recording");
    wait_attached(&rt, &id, true);
    type_into(&mut d, b"last\r", &mut typed);
    rt.ok(&["detach", &id]);
    assert_eq!(d.exit_code(), 0);
    let pid = rt.meta(&id)["pid"].as_i64().unwrap() as i32;
    let start = rt.meta(&id)["pidStart"].as_str().unwrap().to_owned();
    rt.ok(&["kill", &id]);
    assert!(common::wait_gone(pid, &start));
    assert_eq!(
        String::from_utf8_lossy(&recorded(&rt)),
        String::from_utf8_lossy(&typed)
    );
    let _ = std::fs::remove_file(rec_path(&rt));
    drop((b, c));
}

#[test]
fn detach_key_forms_and_none() {
    let rt = Rt::new();
    let (mut a, id) = new_recorder(&rt, &[]);
    // The kitty keyboard protocol's form; what came before it in the same
    // read still arrives.
    a.type_bytes(b"ab\x1b[92;5u");
    assert_eq!(a.exit_code(), 0);
    wait_recorded(&rt, b"ab");
    // modifyOtherKeys' form.
    let mut b = Pty::spawn(&rt, &["attach", &id], 80, 24);
    b.expect(0, b"recording");
    wait_attached(&rt, &id, true);
    b.type_bytes(b"\x1b[27;5;92~");
    assert_eq!(b.exit_code(), 0);
    // Another key.
    let mut c = Pty::spawn(&rt, &["attach", &id, "--detach-key", "ctrl-]"], 80, 24);
    c.expect(0, b"recording");
    wait_attached(&rt, &id, true);
    c.type_bytes(b"\x1ccd\x1d");
    assert_eq!(c.exit_code(), 0);
    wait_recorded(&rt, b"ab\x1ccd");
    // None: Ctrl-\ is typed into the program.
    let mut d = Pty::spawn(&rt, &["attach", &id, "--detach-key", "none"], 80, 24);
    d.expect(0, b"recording");
    wait_attached(&rt, &id, true);
    d.type_bytes(b"\x1c\x1b[92;5u");
    wait_recorded(&rt, b"ab\x1ccd\x1c\x1b[92;5u");
    assert!(
        d.wait(Duration::from_millis(300)).is_none(),
        "still attached"
    );
    let _ = std::fs::remove_file(rec_path(&rt));
}

#[test]
fn the_epilogue_is_written_when_the_client_leaves() {
    let rt = Rt::new();
    let (id, mut t) = new_timer_detached(&rt);
    let mut a = Pty::spawn(&rt, &["attach", &id], 72, 20);
    wait_attached(&rt, &id, true);
    t.round("text:marker");
    a.expect(0, b"marker");
    a.type_bytes(b"\x1c");
    assert_eq!(a.exit_code(), 0);
    a.expect(0, b"[perch] Detached.\r\n");
    let out = a.output();
    let mut want = restore::epilogue(72);
    want.extend_from_slice(b"\r\n[perch] Detached.\r\n");
    assert!(
        out.ends_with(&want),
        "{:?}",
        String::from_utf8_lossy(&out[out.len().saturating_sub(want.len() + 40)..])
    );
    // And when the program exits.
    let mut b = Pty::spawn(&rt, &["attach", &id], 72, 20);
    wait_attached(&rt, &id, true);
    t.send("exit:0");
    assert_eq!(b.exit_code(), 0);
    b.expect(0, b"exit code 0.");
    let out = b.output();
    let at = find(&out, &restore::epilogue(72)).expect("the epilogue");
    assert!(contains(
        &out[at..],
        b"[perch] The program ended: exit code 0."
    ));
}

#[test]
fn nesting_is_refused_and_perch_session_id_is_checked() {
    let rt = Rt::new();
    let perch = s(&common::perch_bin()).to_owned();
    let mut a = Pty::spawn(&rt, &["new", "--", "/bin/sh"], 80, 24);
    let id = only_session(&rt);
    wait_attached(&rt, &id, true);
    a.type_bytes(format!("PS1='$ '; {perch} attach {id}; echo nested=$?\r").as_bytes());
    a.expect(0, b"nested=1");
    assert!(contains(&a.output(), b"from inside itself"));
    // `perch detach` inside the session detaches its own client.
    a.type_bytes(format!("{perch} detach\r").as_bytes());
    assert_eq!(a.exit_code(), 0);
    a.expect(0, b"[perch] Detached.");
    // A process outside the session with the variable set is not trusted.
    let out = rt
        .cmd()
        .env("PERCH_SESSION_ID", &id)
        .args(["detach"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not in that session"),
        "{}",
        common::describe(&out)
    );
    // Nor is it refused an attach.
    let (_r, answer) = Recorder::attach(&rt, &id, json!({}));
    assert_eq!(answer.kind, kind::ATTACHED);
}

/// The bytes each oversized kind writes as its body, which must never
/// reach a client that attached in its middle.
fn body(kind: &str) -> &'static [u8] {
    match kind {
        "apc" | "osc52" | "osc1337" | "osc5522" => b"QUFBQUFB",
        "pm" => b"pmpmpmpm",
        "sos" => b"sosssoss",
        "dcs" => b"#0;2#0;2",
        "csi" => b"1;1;1;1\n1;1;1;1",
        _ => unreachable!(),
    }
}

fn attach_mid_sequence(kind: &str) {
    let rt = Rt::new();
    let (id, mut t) = new_timer_detached(&rt);
    t.round(&format!("text:before open:{kind}:17000000"));
    let (mut r, answer) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    assert_eq!(answer.kind, kind::ATTACHED);
    t.round(&format!("body:{kind}:100000 close:{kind} text:after"));
    r.expect(b"after\r\n");
    r.settle(Duration::from_millis(200));
    assert!(
        !contains(&r.output, body(kind)),
        "{kind}: the client got part of the sequence"
    );
    let i = info(&rt, &id);
    if kind == "csi" {
        // A CSI can change the screen: fed through, the model is partial,
        // and the client got the partial restore.
        assert_eq!(i["model"], "partial");
        assert!(contains(&r.output, restore::NOTICE));
    } else {
        assert_eq!(i["model"], "ok");
        assert!(contains(&r.output, b"before"), "{kind}: the restore");
    }
    // Ordinary output keeps flowing.
    t.round("text:later");
    r.expect(b"later\r\n");
}

#[test]
fn attaching_mid_apc() {
    attach_mid_sequence("apc");
}

#[test]
fn attaching_mid_pm() {
    attach_mid_sequence("pm");
}

#[test]
fn attaching_mid_sos() {
    attach_mid_sequence("sos");
}

#[test]
fn attaching_mid_dcs() {
    attach_mid_sequence("dcs");
}

#[test]
fn attaching_mid_osc52() {
    attach_mid_sequence("osc52");
}

#[test]
fn attaching_mid_osc1337() {
    attach_mid_sequence("osc1337");
}

#[test]
fn attaching_mid_osc5522() {
    attach_mid_sequence("osc5522");
}

#[test]
fn attaching_mid_csi_with_line_feeds() {
    attach_mid_sequence("csi");
}

#[test]
fn queries_go_to_the_live_client_and_back_to_the_supervisor_on_detach() {
    let rt = Rt::new();
    let (id, mut t) = new_timer_detached(&rt);
    let (mut r, _) = Recorder::attach(&rt, &id, json!({"restore": "screen"}));
    r.settle(Duration::from_millis(200));
    // The client's terminal answers: the supervisor does not.
    let from = r.output.len();
    t.round("da1");
    r.until(SECS, |r| contains(&r.output[from..], b"\x1b[c"));
    let rounds = std::fs::read_to_string(&t.out).unwrap();
    let last: Value = serde_json::from_str(rounds.lines().last().unwrap()).unwrap();
    assert_eq!(
        last[0]["reply"],
        Value::Null,
        "nobody answered for the client"
    );
    // A reply from the client reaches the program as typed.
    r.input(b"\x1b[?62c");
    r.close();
    wait_attached(&rt, &id, false);
    // Detached, the supervisor answers again.
    t.round("da1");
    let rounds = std::fs::read_to_string(&t.out).unwrap();
    let last: Value = serde_json::from_str(rounds.lines().last().unwrap()).unwrap();
    assert_eq!(last[0]["reply"], "\u{1b}[?1;2c");
}
