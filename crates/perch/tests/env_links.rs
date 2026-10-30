// SPDX-License-Identifier: GPL-3.0-only

//! Linked variables with two `ssh-agent`s of the test's own: the session's
//! `SSH_AUTH_SOCK` is a link that follows the attached client, so `ssh-add`
//! inside the session reaches the agent of whoever attached last, fails
//! cleanly for a client without one, and a variable unset at creation
//! stays unset. The user's own agent is never involved: every agent and
//! key lives in the test's directory.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::session::{SECS, send_ok};
use common::term::{Pty, Recorder};
use common::{Rt, wait_for};
use serde_json::json;

/// An `ssh-agent` on a socket of the test's own, killed when dropped.
struct Agent {
    sock: PathBuf,
    child: Child,
}

impl Agent {
    fn start(sock: PathBuf) -> Agent {
        let child = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(&sock)
            .env_remove("SSH_AUTH_SOCK")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("run ssh-agent");
        wait_for(SECS, || sock.exists().then_some(())).expect("the agent's socket");
        Agent { sock, child }
    }

    fn add(&self, key: &Path) {
        let out = Command::new("ssh-add")
            .arg(key)
            .env("SSH_AUTH_SOCK", &self.sock)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", common::describe(&out));
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.sock);
    }
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs `ssh-add -l` in the session's shell and returns its exit status
/// and output, and what the shell sees in the two variables.
fn ssh_add_in_session(rt: &Rt, id: &str, n: &mut usize) -> (i32, String, String) {
    *n += 1;
    let out = rt.dir(id).join(format!("check-{n}"));
    let o = out.to_str().unwrap();
    let cmd = format!(
        "ssh-add -l > {o}.txt 2>&1; echo \"$? $SSH_AUTH_SOCK ${{NOT_SET_AT_START-unset}}\" > {o}.tmp; mv {o}.tmp {o}"
    );
    send_ok(rt, id, &["--text", &cmd, "--submit"]);
    let status = rt.wait_file(&out);
    let mut parts = status.split_whitespace();
    let code = parts.next().unwrap().parse().unwrap();
    let vars = parts.collect::<Vec<_>>().join(" ");
    let text = std::fs::read_to_string(format!("{o}.txt")).unwrap();
    (code, text, vars)
}

fn link(rt: &Rt, id: &str) -> PathBuf {
    std::fs::read_link(rt.dir(id).join("env/SSH_AUTH_SOCK")).unwrap()
}

#[test]
fn ssh_auth_sock_follows_the_attached_client() {
    if !["ssh-agent", "ssh-add", "ssh-keygen"]
        .iter()
        .all(|t| have(t))
    {
        eprintln!("skipped: ssh-agent, ssh-add or ssh-keygen is not installed");
        return;
    }
    let rt = Rt::new();
    // Agent A holds a key; agent B holds none.
    let key = rt.path.join("key");
    let out = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "perch-test-key",
            "-f",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::describe(&out));
    let a = Agent::start(rt.path.with_extension("a"));
    let b = Agent::start(rt.path.with_extension("b"));
    a.add(&key);
    let (sa, sb) = (a.sock.to_str().unwrap(), b.sock.to_str().unwrap());

    // Created with A, linking SSH_AUTH_SOCK and a variable that is not set.
    let out = rt
        .cmd()
        .env("SSH_AUTH_SOCK", sa)
        .env_remove("NOT_SET_AT_START")
        .args([
            "new",
            "-d",
            "--link-env",
            "SSH_AUTH_SOCK",
            "--link-env",
            "NOT_SET_AT_START",
            "--",
            "/bin/sh",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::describe(&out));
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert_eq!(rt.meta(&id)["linkedEnv"], json!(["SSH_AUTH_SOCK"]));
    assert_eq!(link(&rt, &id), a.sock);
    let linked = rt.dir(&id).join("env/SSH_AUTH_SOCK");
    let mut n = 0;
    let (code, text, vars) = ssh_add_in_session(&rt, &id, &mut n);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("perch-test-key"), "{text}");
    assert_eq!(vars, format!("{} unset", linked.display()));

    // A client with agent B attaches: the same path now reaches B.
    let (r, _) = Recorder::attach(&rt, &id, json!({"env": {"SSH_AUTH_SOCK": sb}}));
    assert_eq!(link(&rt, &id), b.sock);
    let (code, text, vars) = ssh_add_in_session(&rt, &id, &mut n);
    assert_eq!(code, 1, "B has no identities: {text}");
    assert_eq!(vars, format!("{} unset", linked.display()));
    // Detaching leaves the link alone.
    r.close();
    wait_for(SECS, || {
        let i = rt.ok(&["info", &id, "--json"]);
        i.contains("\"attached\": false").then_some(())
    })
    .expect("detached");
    assert_eq!(link(&rt, &id), b.sock);

    // A client without the variable: the link points nowhere, and ssh-add
    // fails to connect rather than reaching a stale agent.
    let (r, _) = Recorder::attach(&rt, &id, json!({"env": {}}));
    assert!(!rt.dir(&id).join("env/SSH_AUTH_SOCK").exists());
    let (code, text, _) = ssh_add_in_session(&rt, &id, &mut n);
    assert_eq!(code, 2, "no agent: {text}");

    // Takeover re-points to the new client, here `perch attach` itself,
    // which sends its own SSH_AUTH_SOCK.
    let p = Pty::spawn_env(
        &rt,
        &["attach", &id],
        80,
        24,
        &[("SSH_AUTH_SOCK", sa), ("NOT_SET_AT_START", "/elsewhere")],
    );
    wait_for(Duration::from_secs(15), || {
        (std::fs::read_link(rt.dir(&id).join("env/SSH_AUTH_SOCK")).ok()? == a.sock).then_some(())
    })
    .expect("the link follows the new client");
    assert!(!rt.dir(&id).join("env/NOT_SET_AT_START").exists());
    let (code, text, vars) = ssh_add_in_session(&rt, &id, &mut n);
    assert_eq!(code, 0, "{text}");
    assert_eq!(vars, format!("{} unset", linked.display()));
    drop((r, p));
}
