// SPDX-License-Identifier: GPL-3.0-only

//! The detached supervisor with real processes: starting, what the program
//! inherits, ending sessions, tombstones, liveness and staying published.

#![cfg(feature = "test-hooks")]

mod common;

use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Rt, alive, describe, dir_locked, golden, normalize, testprog, wait_for, wait_gone};
use perch::sys;
use serde_json::Value;

fn s(p: &Path) -> &str {
    p.to_str().expect("UTF-8 path")
}

/// A running session whose program writes its inherited state to
/// `{dir}/state.json`, and that state.
fn proc_state(rt: &Rt, extra: &[&str], cmd: &mut Command) -> (String, Value) {
    let prog = testprog("proc-state");
    cmd.args(["new", "-d"])
        .args(extra)
        .args(["--", s(&prog), "{dir}/state.json"]);
    let out = cmd.output().expect("run perch new");
    assert!(out.status.success(), "{}", describe(&out));
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    let text = rt.wait_file(&rt.dir(&id).join("state.json"));
    (id, serde_json::from_str(&text).expect("state JSON"))
}

#[test]
fn golden_meta_json() {
    let rt = Rt::new();
    let id = rt.new_session(&[
        "--name",
        "R: thesis",
        "--kind",
        "r",
        "--owner",
        "jbearak.raven",
        "--annotate",
        "raven.console=ipc",
        "--",
        "sleep",
        "1000",
    ]);
    let mut m = rt.meta(&id);
    assert_eq!(m["id"], id.as_str());
    assert_eq!(m["state"], "running");
    assert_eq!(m["perchVersion"], perch::VERSION);
    assert_eq!(m["host"], sys::hostname().as_str());
    let pid = m["pid"].as_i64().unwrap() as i32;
    assert_eq!(
        perch::procinfo::start_time(pid).as_deref(),
        m["pidStart"].as_str()
    );
    let sup = m["supervisorPid"].as_i64().unwrap() as i32;
    assert_eq!(
        perch::procinfo::start_time(sup).as_deref(),
        m["supervisorStart"].as_str()
    );
    normalize(
        &mut m,
        &[
            "id",
            "cwd",
            "host",
            "createdAt",
            "supervisorPid",
            "supervisorStart",
            "pid",
            "pidStart",
            "perchVersion",
        ],
    );
    golden("meta.json", &format!("{m:#}\n"));
    // The file itself is private.
    let st = sys::lstat(&rt.dir(&id).join("meta.json")).unwrap();
    assert_eq!(st.mode & 0o777, 0o600);
    assert_eq!(
        sys::lstat(&rt.dir(&id).join("sock")).unwrap().mode & 0o777,
        0o600
    );
    assert_eq!(sys::lstat(&rt.dir(&id)).unwrap().mode & 0o777, 0o700);
}

#[test]
fn golden_list_json() {
    let rt = Rt::new();
    let running = rt.new_session(&["--name", "a running one", "--", "sleep", "1000"]);
    let exited = rt.new_session(&[
        "--name",
        "b exited one",
        "--kind",
        "shell",
        "--",
        s(&testprog("exit-with")),
        "7",
    ]);
    wait_for(Duration::from_secs(10), || {
        (rt.meta(&exited)["state"] == "exited").then_some(())
    })
    .expect("exits");
    let mut v = rt.list(true);
    let sessions = v["sessions"].as_array_mut().unwrap();
    sessions.sort_by_key(|x| x["name"].as_str().unwrap_or("").to_owned());
    assert_eq!(sessions[0]["id"], running.as_str());
    assert_eq!(sessions[0]["dir"], s(&rt.dir(&running)));
    normalize(
        &mut v,
        &[
            "id",
            "dir",
            "cwd",
            "host",
            "createdAt",
            "endedAt",
            "supervisorPid",
            "supervisorStart",
            "pid",
            "pidStart",
            "lastOutputAt",
            "perchVersion",
            "command",
        ],
    );
    golden("list.json", &format!("{v:#}\n"));
    // Without --all the tombstone is hidden.
    let v = rt.list(false);
    let ids: Vec<&str> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [running.as_str()]);
}

#[test]
fn exec_failure_is_reported_and_leaves_nothing() {
    let rt = Rt::new();
    let out = rt.perch(&["new", "-d", "--", "/nonexistent/program"]);
    assert_eq!(out.status.code(), Some(127), "{}", describe(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot run `/nonexistent/program`"), "{err}");

    let out = rt.perch(&["new", "-d", "--", "no-such-program-on-path-xyz"]);
    assert_eq!(out.status.code(), Some(127), "{}", describe(&out));

    let noexec = rt.path.join(".noexec");
    std::fs::write(&noexec, "#!/bin/sh\n").unwrap();
    let out = rt.perch(&["new", "-d", "--", s(&noexec)]);
    assert_eq!(out.status.code(), Some(126), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ermission denied"),
        "{}",
        describe(&out)
    );

    let out = rt.perch(&["new", "-d", "--cwd", "/nonexistent/dir", "--", "sleep", "1"]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));

    assert_eq!(rt.list(true)["sessions"], serde_json::json!([]));
}

#[test]
fn program_gets_the_creators_umask_and_directory_and_clean_signals() {
    let rt = Rt::new();
    let work = rt.path.join(".work");
    std::fs::create_dir(&work).unwrap();
    let mut cmd = rt.cmd();
    // SAFETY: the closure makes only async-signal-safe calls.
    unsafe {
        cmd.pre_exec(|| {
            sys::set_umask(0o027);
            sys::block_signals(&[sys::sig::USR1, sys::sig::INT]);
            sys::ignore_signal(sys::sig::QUIT)?;
            Ok(())
        });
    }
    cmd.current_dir(&work).env("PWD", &work);
    let (id, st) = proc_state(&rt, &[], &mut cmd);
    assert_eq!(st["umask"], 0o027);
    assert_eq!(
        Path::new(st["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        work.canonicalize().unwrap()
    );
    assert_eq!(rt.meta(&id)["cwd"], s(&work));
    // The supervisor ignores SIGHUP and SIGPIPE; the program must not.
    assert_eq!(st["blocked"], serde_json::json!([]), "{st:#}");
    assert_eq!(st["nonDefault"], serde_json::json!([]), "{st:#}");
    assert_eq!(st["sessionLeader"], true);
    assert_eq!(st["isatty"], true);

    // --cwd, relative to the creator's directory.
    std::fs::create_dir(work.join("sub")).unwrap();
    let mut cmd = rt.cmd();
    cmd.current_dir(&work).env("PWD", &work);
    let (id, st) = proc_state(&rt, &["--cwd", "sub"], &mut cmd);
    assert_eq!(
        Path::new(st["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        work.join("sub").canonicalize().unwrap()
    );
    assert_eq!(rt.meta(&id)["cwd"], s(&work.join("sub")));
}

#[test]
fn program_sees_only_descriptors_0_1_2() {
    let rt = Rt::new();
    let mut cmd = rt.cmd();
    // Leak descriptors into `perch new` without close-on-exec, including
    // copies of its stdout pipe, which would also keep `output()` waiting.
    // SAFETY: async-signal-safe calls only.
    unsafe {
        cmd.pre_exec(|| {
            sys::dup2(1, 9)?;
            sys::dup2(2, 200)?;
            Ok(())
        });
    }
    let prog = testprog("fd-lister");
    let out = cmd
        .args(["new", "-d", "--", s(&prog), "{dir}/fds"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", describe(&out));
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    let fds = rt.wait_file(&rt.dir(&id).join("fds"));
    assert_eq!(fds, "0\n1\n2\n");
}

#[test]
fn pty_size_clamping_and_termios() {
    let rt = Rt::new();
    // No terminal: 80 by 24 with IUTF8.
    let (_, st) = proc_state(&rt, &[], &mut rt.cmd());
    assert_eq!(
        (st["rows"].as_u64(), st["cols"].as_u64()),
        (Some(24), Some(80))
    );
    assert_eq!(st["iutf8"], true);
    assert_eq!(st["echo"], true);

    // A creator terminal: its size, clamped to 1..500, and its termios.
    let creator = |rows: u16, cols: u16| {
        let (master, path) = sys::open_pty().unwrap();
        let slave: OwnedFd = sys::open_rw_noctty(&path).unwrap();
        let mut t = sys::tcgetattr(std::os::fd::AsRawFd::as_raw_fd(&slave)).unwrap();
        t.set_iutf8(true);
        t.set_echo(false);
        sys::tcsetattr(std::os::fd::AsRawFd::as_raw_fd(&slave), &t).unwrap();
        sys::set_winsize(
            std::os::fd::AsRawFd::as_raw_fd(&master),
            sys::Winsize { rows, cols },
        )
        .unwrap();
        let mut cmd = rt.cmd();
        cmd.stdin(Stdio::from(slave));
        (master, cmd)
    };
    for ((rows, cols), (want_rows, want_cols)) in [
        ((0, 1000), (1, 500)),
        ((700, 3), (500, 3)),
        ((50, 132), (50, 132)),
    ] {
        let (_master, mut cmd) = creator(rows, cols);
        let (_, st) = proc_state(&rt, &[], &mut cmd);
        assert_eq!(
            (st["rows"].as_u64(), st["cols"].as_u64()),
            (Some(want_rows), Some(want_cols)),
            "creator {rows}x{cols}"
        );
        assert_eq!(st["iutf8"], true);
        assert_eq!(st["echo"], false, "termios copied from the creator");
        assert_eq!(st["icanon"], true);
    }
}

#[test]
fn environment_and_placeholders() {
    let rt = Rt::new();
    let mut cmd = rt.cmd();
    cmd.env("CREATOR_VAR", "from-creator")
        .env("PERCH_SESSION_ID", "stale");
    let (id, st) = proc_state(
        &rt,
        &[
            "--env",
            "IPC={dir}/ipc",
            "--env",
            "BRACES={{x}} and }}{{",
            "--env",
            "MIXED={id}:{{{id}}}",
            "--env",
            "CREATOR_VAR=overridden",
        ],
        &mut cmd,
    );
    let dir = rt.dir(&id);
    let env = &st["env"];
    assert_eq!(env["PERCH_SESSION_ID"], id.as_str());
    assert_eq!(env["PERCH_SESSION_DIR"], s(&dir));
    assert_eq!(env["IPC"], format!("{}/ipc", s(&dir)));
    assert_eq!(env["BRACES"], "{x} and }{");
    assert_eq!(env["MIXED"], format!("{id}:{{{id}}}"));
    assert_eq!(env["CREATOR_VAR"], "overridden");
    assert_eq!(
        env["PERCH_TEST_TIME_SCALE"],
        rt.scale.as_str(),
        "the creator's environment passes through"
    );
    // The command's placeholders were expanded too: the state file is there.
    let cmd0 = rt.meta(&id)["command"][1].clone();
    assert_eq!(cmd0, format!("{}/state.json", s(&dir)));

    // A bad placeholder is refused before anything starts.
    for bad in [["--env", "X={nope}"], ["--env", "X=a { b"]] {
        let mut args = vec!["new", "-d"];
        args.extend(bad);
        args.extend(["--", "sleep", "1"]);
        let out = rt.perch(&args);
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
        assert!(String::from_utf8_lossy(&out.stderr).contains("placeholder"));
    }
    let out = rt.perch(&["new", "-d", "--", "echo", "{dir"]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert_eq!(rt.list(true)["sessions"].as_array().unwrap().len(), 1);
}

#[test]
fn scrollback_has_a_ceiling() {
    let rt = Rt::new();
    let a = rt.new_session(&["--scrollback", "50000", "--", "sleep", "1000"]);
    let b = rt.new_session(&["--", "sleep", "1000"]);
    let c = rt.new_session(&["--scrollback", "20", "--", "sleep", "1000"]);
    assert_eq!(rt.meta(&a)["scrollback"], 10_000);
    assert_eq!(rt.meta(&b)["scrollback"], 1000);
    assert_eq!(rt.meta(&c)["scrollback"], 20);
    let out = rt.perch(&["new", "-d", "--scrollback", "lots", "--", "sleep", "1"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn start_times_with_parentheses_and_spaces_in_the_name() {
    let rt = Rt::new();
    let bin = rt.path.join(".bin");
    std::fs::create_dir(&bin).unwrap();
    let odd = bin.join("a) b (c");
    std::os::unix::fs::symlink(testprog("fd-lister"), &odd).unwrap();
    let id = rt.new_session(&["--", s(&odd), "{dir}/fds"]);
    let m = rt.meta(&id);
    let pid = m["pid"].as_i64().unwrap() as i32;
    let start = m["pidStart"].as_str().unwrap().to_owned();
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        assert!(stat.contains("(a) b (c)"), "{stat}");
        assert_eq!(
            perch::procinfo::start_time_from_stat(&stat).as_deref(),
            Some(start.as_str())
        );
    }
    assert_eq!(
        perch::procinfo::start_time(pid).as_deref(),
        Some(start.as_str())
    );
    assert_eq!(
        perch::procinfo::start_time(pid).as_deref(),
        Some(start.as_str())
    );
    // kill --pid accepts it, because the start time reads back exactly.
    rt.ok(&["kill", "--pid", &id]);
    assert!(wait_gone(pid, &start));
}

/// A shell with job control, a background job, a stopped job and a child
/// that ignores SIGHUP. Returns the session and every PID with its start.
fn jobs_session(rt: &Rt) -> (String, Vec<(i32, String)>) {
    let script = r#"set -m
sh -c 'trap "" HUP; echo $$ > "$PERCH_SESSION_DIR/ignorer.tmp"; mv "$PERCH_SESSION_DIR/ignorer.tmp" "$PERCH_SESSION_DIR/ignorer"; exec sleep 1000' &
sleep 1000 &
echo $! > "$PERCH_SESSION_DIR/bg"
sleep 1000 &
p=$!
kill -STOP $p
echo $p > "$PERCH_SESSION_DIR/stopped"
echo $$ > "$PERCH_SESSION_DIR/shell"
wait
sleep 1000
"#;
    let id = rt.new_session(&["--", "sh", "-c", script]);
    let mut pids = Vec::new();
    for f in ["ignorer", "bg", "stopped", "shell"] {
        let text = rt.wait_file(&rt.dir(&id).join(f));
        let pid: i32 = text.trim().parse().unwrap();
        let start = perch::procinfo::start_time(pid).expect("running");
        pids.push((pid, start));
    }
    // The jobs are in their own process groups but the same session.
    let shell = pids[3].0;
    for (pid, _) in &pids {
        assert_eq!(sys::getsid(*pid).unwrap(), shell);
    }
    (id, pids)
}

#[test]
fn kill_reaches_background_and_stopped_jobs_and_sighup_ignorers() {
    let rt = Rt::with_scale("0.4"); // a 2-second grace
    let (id, pids) = jobs_session(&rt);
    let t = Instant::now();
    rt.ok(&["kill", &id]);
    let took = t.elapsed();
    for (pid, start) in &pids {
        assert!(wait_gone(*pid, start), "pid {pid} survived the kill");
    }
    // Everything but the ignorer ended on SIGHUP; the ignorer needed the
    // SIGKILL after the grace period.
    assert!(took >= Duration::from_millis(1800), "kill took {took:?}");
    assert!(!rt.dir(&id).exists(), "kill leaves no tombstone");
}

#[test]
fn kill_force_skips_the_grace_period() {
    let rt = Rt::with_scale("1");
    let (id, pids) = jobs_session(&rt);
    let t = Instant::now();
    rt.ok(&["kill", "--force", &id]);
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    for (pid, start) in &pids {
        assert!(wait_gone(*pid, start), "pid {pid} survived");
    }
    assert!(!rt.dir(&id).exists());
}

#[test]
fn sigterm_to_the_supervisor_is_a_kill_with_a_tombstone() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let m = rt.meta(&id);
    let sup = m["supervisorPid"].as_i64().unwrap() as i32;
    let pid = m["pid"].as_i64().unwrap() as i32;
    sys::kill(sup, sys::sig::TERM).unwrap();
    assert!(wait_gone(sup, m["supervisorStart"].as_str().unwrap()));
    assert!(!alive(pid, m["pidStart"].as_str().unwrap()));
    let m = rt.meta(&id);
    assert_eq!(m["state"], "exited");
    assert_eq!(m["exit"]["signal"], "SIGHUP");
}

#[test]
fn lost_after_the_supervisor_is_killed_with_its_log_kept() {
    let rt = Rt::with_scale("0.3");
    let id = rt.new_session(&["--", s(&testprog("fd-lister")), "{dir}/fds"]);
    let m = rt.meta(&id);
    let sup = m["supervisorPid"].as_i64().unwrap() as i32;
    assert!(dir_locked(&rt.dir(&id)));
    sys::kill(sup, sys::sig::KILL).unwrap();
    wait_for(Duration::from_secs(5), || {
        (!dir_locked(&rt.dir(&id))).then_some(())
    })
    .expect("lock released");
    // A directory younger than 10 seconds (3 here) is shown as lost but
    // not rewritten.
    assert_eq!(rt.listed(&id).expect("listed")["state"], "lost");
    assert_eq!(rt.meta(&id)["state"], "running");
    std::thread::sleep(Duration::from_millis(3100));
    let listed = rt.listed(&id).expect("listed with --all");
    assert_eq!(listed["state"], "lost");
    assert_eq!(rt.meta(&id)["state"], "lost");
    assert!(rt.meta(&id)["endedAt"].is_string());
    assert!(rt.list(false)["sessions"].as_array().unwrap().is_empty());
    let log = std::fs::read_to_string(rt.dir(&id).join("supervisor.log")).unwrap();
    assert!(log.contains("program") && log.contains("running"), "{log}");
    // The program lost its terminal and got SIGHUP.
    assert!(wait_gone(
        m["pid"].as_i64().unwrap() as i32,
        m["pidStart"].as_str().unwrap()
    ));
    // `info` reads a tombstone; `kill` refuses it; `dismiss` removes it.
    let info: Value = serde_json::from_str(&rt.ok(&["info", &id, "--json"])).unwrap();
    assert_eq!(info["state"], "lost");
    assert_eq!(rt.perch(&["kill", &id]).status.code(), Some(1));
    rt.ok(&["dismiss", &id]);
    assert!(!rt.dir(&id).exists());
}

#[test]
fn a_stopped_supervisor_is_unresponsive_within_500_ms() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let m = rt.meta(&id);
    let sup = m["supervisorPid"].as_i64().unwrap() as i32;
    sys::kill(sup, sys::sig::STOP).unwrap();
    let t = Instant::now();
    let v = rt.list(false);
    let took = t.elapsed();
    let _ = sys::kill(sup, sys::sig::CONT);
    assert_eq!(v["sessions"][0]["state"], "unresponsive", "{v:#}");
    assert!(took >= Duration::from_millis(450), "{took:?}");
    assert!(took < Duration::from_millis(1200), "list took {took:?}");
    let v = rt.list(false);
    assert_eq!(v["sessions"][0]["state"], "running");
}

#[test]
fn kill_by_pid_checks_start_times() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let meta_path = rt.dir(&id).join("meta.json");
    let original = std::fs::read_to_string(&meta_path).unwrap();
    let m: Value = serde_json::from_str(&original).unwrap();
    let (pid, start) = (
        m["pid"].as_i64().unwrap() as i32,
        m["pidStart"].as_str().unwrap().to_owned(),
    );
    let (sup, sup_start) = (
        m["supervisorPid"].as_i64().unwrap() as i32,
        m["supervisorStart"].as_str().unwrap().to_owned(),
    );

    let tamper = |key: &str| {
        let mut bad = m.clone();
        bad[key] = Value::String(format!(
            "{}:1",
            if cfg!(target_os = "linux") {
                "linux"
            } else {
                "macos"
            }
        ));
        let tmp = rt.dir(&id).join(".tamper");
        std::fs::write(&tmp, bad.to_string()).unwrap();
        std::fs::rename(&tmp, &meta_path).unwrap();
    };
    for key in ["pidStart", "supervisorStart"] {
        tamper(key);
        let out = rt.perch(&["kill", "--pid", &id]);
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("different start time"),
            "{}",
            describe(&out)
        );
        assert!(
            alive(pid, &start) && alive(sup, &sup_start),
            "nothing was signaled"
        );
    }
    std::fs::write(&meta_path, &original).unwrap();
    rt.ok(&["kill", "--pid", &id]);
    assert!(wait_gone(pid, &start));
    assert!(wait_gone(sup, &sup_start));
    assert!(!rt.dir(&id).exists());
}

#[test]
fn kill_by_pid_reaches_a_stopped_supervisor() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let m = rt.meta(&id);
    let sup = m["supervisorPid"].as_i64().unwrap() as i32;
    sys::kill(sup, sys::sig::STOP).unwrap();
    assert_eq!(
        rt.perch(&["kill", &id]).status.code(),
        Some(1),
        "a request cannot reach it"
    );
    rt.ok(&["kill", "--pid", "--force", &id]);
    assert!(wait_gone(sup, m["supervisorStart"].as_str().unwrap()));
    assert!(wait_gone(
        m["pid"].as_i64().unwrap() as i32,
        m["pidStart"].as_str().unwrap()
    ));
    assert!(!rt.dir(&id).exists());
}

#[test]
fn tombstone_on_exit_while_detached() {
    let rt = Rt::new();
    let code = rt.new_session(&[
        "--name",
        "code",
        "--",
        s(&testprog("exit-with")),
        "3",
        "300",
    ]);
    let sig = rt.new_session(&[
        "--name",
        "sig",
        "--",
        s(&testprog("exit-with")),
        "signal",
        "SIGTERM",
        "300",
    ]);
    for id in [&code, &sig] {
        wait_for(Duration::from_secs(10), || {
            (!dir_locked(&rt.dir(id))).then_some(())
        })
        .expect("supervisor exits");
    }
    let m = rt.meta(&code);
    assert_eq!(m["state"], "exited");
    assert_eq!(m["exit"], serde_json::json!({ "code": 3 }));
    let m = rt.meta(&sig);
    assert_eq!(
        m["exit"],
        serde_json::json!({ "signal": "SIGTERM", "signalNumber": 15 })
    );
    for id in [&code, &sig] {
        let dir = rt.dir(id);
        assert_eq!(
            std::fs::read_to_string(dir.join("tail.txt")).unwrap(),
            "",
            "empty until the model"
        );
        assert!(!dir.join("sock").exists());
        assert!(dir.join("supervisor.log").exists());
    }
    assert!(rt.list(false)["sessions"].as_array().unwrap().is_empty());
    let text = rt.ok(&["list", "--all"]);
    assert!(text.contains("exited (exit code 3)"), "{text}");
    assert!(text.contains("killed by signal 15 (SIGTERM)"), "{text}");
    rt.ok(&["dismiss", "code"]);
    rt.ok(&["dismiss", &sig[..6]]);
    assert!(rt.list(true)["sessions"].as_array().unwrap().is_empty());
}

#[test]
fn info_rename_and_annotate() {
    let rt = Rt::new();
    let id = rt.new_session(&[
        "--name",
        "first",
        "--annotate",
        "a=1",
        "--annotate",
        "b=2",
        "--",
        "sleep",
        "1000",
    ]);
    let info: Value = serde_json::from_str(&rt.ok(&["info", "first", "--json"])).unwrap();
    assert_eq!(info["id"], id.as_str());
    assert_eq!(info["state"], "running");
    assert_eq!(info["attached"], false);
    assert_eq!(
        (info["rows"].as_u64(), info["cols"].as_u64()),
        (Some(24), Some(80))
    );
    rt.ok(&["rename", &id, "second"]);
    rt.ok(&["annotate", "second", "c=3", "a=changed", "--unset", "b"]);
    let m = rt.meta(&id);
    assert_eq!(m["name"], "second");
    assert_eq!(
        m["annotations"],
        serde_json::json!({ "a": "changed", "c": "3" })
    );
    let text = rt.ok(&["info", &id[..3]]);
    assert!(
        text.contains("second") && text.contains("annotation:  c=3"),
        "{text}"
    );
    assert_eq!(rt.perch(&["info", "nonexistent"]).status.code(), Some(1));
    assert_eq!(rt.perch(&["rename", &id, ""]).status.code(), Some(2));
}

#[test]
fn survives_its_creators_process_group_and_parent_being_killed() {
    let rt = Rt::new();
    let out = rt.path.join(".id");
    let script = format!(
        "'{}' new -d -- sleep 1000 > '{}'; exec sleep 1000",
        s(&common::perch_bin()),
        s(&out)
    );
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(script)
        .env("PERCH_RUNTIME_DIR", &rt.path)
        .env("PERCH_TEST_TIME_SCALE", &rt.scale)
        .stdin(Stdio::null())
        .process_group(0);
    let mut child = sh.spawn().unwrap();
    let id = wait_for(Duration::from_secs(10), || {
        std::fs::read_to_string(&out)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| t.len() == 8)
    })
    .expect("perch new printed an ID");
    let pgid = child.id() as i32;
    sys::kill(-pgid, sys::sig::HUP).unwrap();
    let _ = sys::kill(-pgid, sys::sig::KILL);
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let listed = rt.listed(&id).expect("still listed");
    assert_eq!(listed["state"], "running");
    let m = rt.meta(&id);
    assert!(alive(
        m["pid"].as_i64().unwrap() as i32,
        m["pidStart"].as_str().unwrap()
    ));
}

#[test]
fn parallel_lists_during_parallel_creation() {
    let rt = Rt::new();
    let fdl = testprog("fd-lister");
    let (ids, lists) = std::thread::scope(|scope| {
        let news: Vec<_> = (0..10)
            .map(|_| scope.spawn(|| rt.new_session(&["--", s(&fdl), "{dir}/fds"])))
            .collect();
        let lists: Vec<_> = (0..20)
            .map(|_| {
                scope.spawn(|| {
                    let out = rt.perch(&["list", "--json"]);
                    assert!(out.status.success(), "{}", describe(&out));
                    serde_json::from_slice::<Value>(&out.stdout).expect("list --json is JSON")
                })
            })
            .collect();
        let ids: Vec<String> = news.into_iter().map(|h| h.join().unwrap()).collect();
        let lists: Vec<Value> = lists.into_iter().map(|h| h.join().unwrap()).collect();
        (ids, lists)
    });
    for v in &lists {
        for sess in v["sessions"].as_array().unwrap() {
            let st = sess["state"].as_str().unwrap();
            assert!(
                ["starting", "running", "unresponsive"].contains(&st),
                "{sess:#}"
            );
        }
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 10);
    let v = rt.list(false);
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 10);
    for sess in sessions {
        assert_eq!(sess["state"], "running");
        assert!(ids.contains(&sess["id"].as_str().unwrap().to_owned()));
    }
}

#[test]
fn a_taken_id_is_refused() {
    let rt = Rt::new();
    rt.new_session(&["--id", "fixedid1", "--", "sleep", "1000"]);
    let out = rt.perch(&["new", "-d", "--id", "fixedid1", "--", "sleep", "1000"]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already exists"),
        "{}",
        describe(&out)
    );
    assert_eq!(rt.list(true)["sessions"].as_array().unwrap().len(), 1);
    // No build directory is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&rt.path)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".new-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn republishes_a_deleted_directory_as_degraded() {
    let rt = Rt::with_scale("0.01"); // identity checks every 0.6 s
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let dir = rt.dir(&id);
    let held = sys::lstat(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let m = wait_for(Duration::from_secs(10), || {
        let m: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
        (m["degraded"] == serde_json::json!(["directory-recreated"])).then_some(m)
    })
    .expect("republished");
    assert_eq!(m["state"], "running");
    assert!(!sys::lstat(&dir).unwrap().same_file(&held));
    assert!(dir_locked(&dir), "the new directory is locked");
    let listed = wait_for(Duration::from_secs(5), || {
        rt.listed(&id).filter(|l| l["state"] == "running")
    })
    .expect("reachable through the new socket");
    assert_eq!(
        listed["degraded"],
        serde_json::json!(["directory-recreated"])
    );
    assert!(
        std::fs::read_to_string(dir.join("supervisor.log"))
            .unwrap()
            .contains("republished")
    );

    // A deleted socket alone is rebound in place.
    std::fs::remove_file(dir.join("sock")).unwrap();
    wait_for(Duration::from_secs(10), || {
        dir.join("sock").exists().then_some(())
    })
    .expect("socket rebound");
    wait_for(Duration::from_secs(5), || {
        rt.listed(&id).filter(|l| l["state"] == "running")
    })
    .expect("reachable");
    rt.ok(&["kill", &id]);
    assert!(!dir.exists());
}

#[test]
fn hourly_touch_reaches_foreign_files_and_sockets() {
    let rt = Rt::with_scale("0.001"); // an hour is 3.6 seconds
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let dir = rt.dir(&id);
    std::fs::write(dir.join("foreign.txt"), "x").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(dir.join("foreign.sock")).unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/deep.txt"), "y").unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", dir.join("dangling")).unwrap();
    let dfd = sys::open_dir(&dir).unwrap();
    let sub = sys::open_dir(&dir.join("sub")).unwrap();
    let old = 1_000_000_000; // 2001
    let names = ["foreign.txt", "foreign.sock", "sub", "dangling"];
    for n in names {
        sys::set_times_at(dfd.as_fd(), n.as_ref(), old).unwrap();
    }
    sys::set_times_at(sub.as_fd(), "deep.txt".as_ref(), old).unwrap();
    let touched = |fd: &OwnedFd, n: &str| {
        sys::fstatat(fd.as_fd(), n.as_ref()).is_ok_and(|st| st.mtime.0 > old + 86_400)
    };
    wait_for(Duration::from_secs(15), || {
        (names.iter().all(|n| touched(&dfd, n)) && touched(&sub, "deep.txt")).then_some(())
    })
    .expect("every entry touched");
    assert!(
        sys::fstatat(dfd.as_fd(), "foreign.sock".as_ref())
            .unwrap()
            .is_socket()
    );
    assert!(
        sys::fstatat(dfd.as_fd(), "dangling".as_ref())
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn directory_flock_survives_rename() {
    let rt = Rt::new();
    let a = rt.path.join(".lock-a");
    let b = rt.path.join(".lock-b");
    std::fs::create_dir(&a).unwrap();
    let held = sys::open_dir(&a).unwrap();
    assert!(sys::try_flock(held.as_fd(), sys::Lock::Exclusive).unwrap());
    std::fs::rename(&a, &b).unwrap();
    let other = sys::open_dir(&b).unwrap();
    assert!(
        !sys::try_flock(other.as_fd(), sys::Lock::Shared).unwrap(),
        "lock follows the directory"
    );
    drop(held);
    // Another test thread's fork can share the descriptor until its exec
    // closes it, so the release may take a moment.
    wait_for(Duration::from_secs(5), || {
        sys::try_flock(other.as_fd(), sys::Lock::Shared)
            .unwrap()
            .then_some(())
    })
    .expect("the lock is released with its last descriptor");

    // A session directory was locked as `.new-*` and then renamed.
    let id = rt.new_session(&["--", "sleep", "1000"]);
    assert!(dir_locked(&rt.dir(&id)));
}

#[test]
fn runtime_directory_refusals() {
    let rt = Rt::new();
    let link = rt.path.with_extension("link");
    std::os::unix::fs::symlink(&rt.path, &link).unwrap();
    let out = rt
        .cmd()
        .env("PERCH_RUNTIME_DIR", &link)
        .args(["list"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&link);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("symbolic link"),
        "{}",
        describe(&out)
    );

    std::fs::set_permissions(
        &rt.path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    for args in [&["list"][..], &["new", "-d", "--", "sleep", "1"]] {
        let out = rt.perch(args);
        assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("mode is 0755"),
            "{}",
            describe(&out)
        );
    }
    std::fs::set_permissions(
        &rt.path,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();

    // A missing runtime directory is created with mode 0700.
    let fresh = rt.path.join(".fresh");
    let out = rt
        .cmd()
        .env("PERCH_RUNTIME_DIR", &fresh)
        .args(["list"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(sys::lstat(&fresh).unwrap().mode & 0o7777, 0o700);
}
