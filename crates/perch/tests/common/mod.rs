// SPDX-License-Identifier: GPL-3.0-only

//! Shared helpers for the integration tests: a private runtime directory
//! under /tmp per test, running `perch`, finding the test programs, and
//! cleaning up every process a test started.

#![allow(dead_code)]

pub mod session;
pub mod term;

use std::ffi::OsStr;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use perch::{procinfo, sys};
use serde_json::Value;

/// The `perch` binary under test.
pub fn perch_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_perch"))
}

/// A program from `perch-testprogs`, found next to the test binary in
/// `target/[<triple>/]<profile>/`. Build them first with
/// `cargo build -p perch-testprogs`, with the same `--target` and features
/// as the test run.
pub fn testprog(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    // target/<profile>/deps/<test-binary>
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>");
    let path = profile_dir.join(name);
    assert!(
        path.is_file(),
        "test program {} is missing. Build the test programs first, with the same target and \
         features as this test run: `cargo build -p perch-testprogs{}`",
        path.display(),
        if cfg!(feature = "test-hooks") {
            " --features perch/test-hooks"
        } else {
            ""
        }
    );
    path
}

/// A runtime directory of the test's own, removed with every session and
/// process in it when dropped.
pub struct Rt {
    pub path: PathBuf,
    /// `PERCH_TEST_TIME_SCALE` for every `perch` this runs.
    pub scale: String,
}

impl Rt {
    /// A fresh runtime directory, `/tmp/pt-<random>`, mode 0700, with
    /// timers at a tenth of their real length.
    pub fn new() -> Rt {
        Rt::with_scale("0.1")
    }

    pub fn with_scale(scale: &str) -> Rt {
        let path = PathBuf::from(format!("/tmp/pt-{}", perch::runtime::random_name(8)));
        sys::mkdir(&path, 0o700).expect("create the runtime directory");
        Rt {
            path,
            scale: scale.to_owned(),
        }
    }

    /// A `perch` command against this runtime directory, with no terminal.
    pub fn cmd(&self) -> Command {
        let mut c = Command::new(perch_bin());
        // `SSH_AUTH_SOCK` is linked by default, so whether the machine
        // running the tests has an agent would change sessions; tests that
        // want one set it.
        c.env("PERCH_RUNTIME_DIR", &self.path)
            .env("PERCH_TEST_TIME_SCALE", &self.scale)
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    pub fn perch(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().expect("run perch")
    }

    /// Runs `perch` and requires success.
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.perch(args);
        assert!(
            out.status.success(),
            "perch {args:?} failed: {}",
            describe(&out)
        );
        String::from_utf8(out.stdout).expect("UTF-8 output")
    }

    /// `perch new -d <args>` and the new session's ID.
    pub fn new_session(&self, args: &[&str]) -> String {
        let mut all = vec!["new", "-d"];
        all.extend_from_slice(args);
        self.ok(&all).trim().to_owned()
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.path.join(id)
    }

    pub fn meta(&self, id: &str) -> Value {
        let text = std::fs::read_to_string(self.dir(id).join("meta.json")).expect("read meta.json");
        serde_json::from_str(&text).expect("meta.json is JSON")
    }

    pub fn list(&self, all: bool) -> Value {
        let mut args = vec!["list", "--json"];
        if all {
            args.push("--all");
        }
        serde_json::from_str(&self.ok(&args)).expect("list --json is JSON")
    }

    /// The session from `list --json --all`.
    pub fn listed(&self, id: &str) -> Option<Value> {
        self.list(true)["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["id"] == id)
            .cloned()
    }

    /// Waits for a file the program writes into the session directory.
    pub fn wait_file(&self, path: &Path) -> String {
        wait_for(Duration::from_secs(10), || {
            std::fs::read_to_string(path).ok()
        })
        .unwrap_or_else(|| panic!("{} never appeared", path.display()))
    }
}

impl Drop for Rt {
    fn drop(&mut self) {
        let uid = sys::getuid();
        if let Ok(entries) = std::fs::read_dir(&self.path) {
            for e in entries.flatten() {
                let Ok(text) = std::fs::read_to_string(e.path().join("meta.json")) else {
                    continue;
                };
                let Ok(m) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let sup = m["supervisorPid"].as_i64().unwrap_or(0) as i32;
                if procinfo::is_same_process(sup, m["supervisorStart"].as_str().unwrap_or(""), uid)
                {
                    let _ = sys::kill(sup, sys::sig::KILL);
                }
                let pid = m["pid"].as_i64().unwrap_or(0) as i32;
                if pid > 0
                    && procinfo::is_same_process(pid, m["pidStart"].as_str().unwrap_or(""), uid)
                {
                    for p in procinfo::session_members(pid) {
                        let _ = sys::kill(p, sys::sig::KILL);
                    }
                    let _ = sys::kill(pid, sys::sig::KILL);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Polls `f` every 20 ms until it returns `Some` or `timeout` passes.
pub fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a process with this PID and start time still runs. A zombie
/// does not count: in a container without an init, nobody may reap it.
pub fn alive(pid: i32, start: &str) -> bool {
    procinfo::start_time(pid).as_deref() == Some(start) && !procinfo::is_zombie(pid)
}

/// Waits for a recorded process to be gone.
pub fn wait_gone(pid: i32, start: &str) -> bool {
    wait_for(Duration::from_secs(10), || {
        (!alive(pid, start)).then_some(())
    })
    .is_some()
}

pub fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Whether some other open description holds a lock on the directory.
pub fn dir_locked(path: &Path) -> bool {
    let fd = sys::open_dir(path).expect("open the directory");
    !sys::try_flock(fd.as_fd(), sys::Lock::Shared).expect("flock")
}

/// Replaces the values of `keys` (where not null) with `"<key>"`, at any
/// depth, so golden files hold no per-run values.
pub fn normalize(v: &mut Value, keys: &[&str]) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if keys.contains(&k.as_str()) && !x.is_null() {
                    *x = Value::String(format!("<{k}>"));
                } else {
                    normalize(x, keys);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| normalize(x, keys)),
        _ => {}
    }
}

/// Compares `actual` with `tests/golden/<name>`, or writes it when
/// `UPDATE_GOLDEN=1`.
pub fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == OsStr::new("1")) {
        std::fs::write(&path, actual).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} differs; run with UPDATE_GOLDEN=1 to accept",
        path.display()
    );
}
