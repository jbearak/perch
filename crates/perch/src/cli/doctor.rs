// SPDX-License-Identifier: GPL-3.0-only

//! `perch doctor [--json]`: what Perch can detect about whether sessions
//! will work and survive on this host.
//!
//! - The runtime directory passes the checks every Perch process makes, is
//!   not one logind removes at logout, and leaves room for socket paths.
//! - `flock` on a directory there really locks (it may not on NFS).
//! - On Linux: this process's cgroup, which a supervisor started from here
//!   shares (a login session's scope, the user's service manager, or a
//!   service whose stop ends everything in it); logind's
//!   `KillUserProcesses` and the user's linger; and whether this is a
//!   container.
//!
//! Host policy is not something Perch can change. A warning means sessions
//! are not expected to survive logout or the end of whatever started them.

use std::ffi::OsString;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

use super::usage_error;
use crate::runtime;
use crate::sys::{self, Lock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Info => "info",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

struct Check {
    name: &'static str,
    status: Status,
    message: String,
    /// Extra fields for `--json`.
    data: Value,
}

fn check(name: &'static str, status: Status, message: impl Into<String>) -> Check {
    Check {
        name,
        status,
        message: message.into(),
        data: Value::Null,
    }
}

pub(super) fn run(args: &[String]) -> ExitCode {
    let json_out = match args {
        [] => false,
        [a] if a == "--json" => true,
        _ => return usage_error("usage: perch doctor [--json]"),
    };
    let mut checks = Vec::new();
    // Whether a session started here is expected to outlive the login.
    let mut survives: Option<bool> = Some(true);

    let path = runtime::default_path();
    let rt = runtime::open(&path);
    match &rt {
        Ok(_) => checks.push(check(
            "runtime-dir",
            Status::Ok,
            format!("{}: a directory of yours, mode 0700", path.display()),
        )),
        Err(e) => checks.push(check("runtime-dir", Status::Fail, e.clone())),
    }
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())
        && path.starts_with(Path::new(&xdg))
    {
        checks.push(check(
            "runtime-dir-location",
            Status::Warn,
            format!(
                "{} is inside $XDG_RUNTIME_DIR, which logind removes when your last login ends",
                path.display()
            ),
        ));
        survives = Some(false);
    }
    // `<runtime>/<8-character id>/sock`.
    let longest = path.as_os_str().len() + "/k3x9q2ab/sock".len();
    let max = sys::max_socket_path();
    checks.push(if longest <= max {
        check(
            "socket-path",
            Status::Ok,
            format!("socket paths are {longest} bytes, within the limit of {max}"),
        )
    } else {
        check(
            "socket-path",
            Status::Fail,
            format!(
                "socket paths would be {longest} bytes, over the limit of {max}; set PERCH_RUNTIME_DIR to a shorter path"
            ),
        )
    });
    if let Ok(rt) = &rt {
        checks.push(flock_check(rt));
    }
    #[cfg(target_os = "linux")]
    linux::checks(&mut checks, &mut survives);
    #[cfg(target_os = "macos")]
    checks.push(check(
        "host",
        Status::Info,
        "macOS has no logind: sessions outlive the terminal or SSH login that started them, but not a reboot",
    ));

    let failed = checks.iter().any(|c| c.status == Status::Fail);
    if json_out {
        let list: Vec<Value> = checks
            .iter()
            .map(|c| {
                let mut v = json!({
                    "name": c.name,
                    "status": c.status.name(),
                    "message": c.message,
                });
                if let Value::Object(extra) = &c.data {
                    v.as_object_mut().expect("object").extend(extra.clone());
                }
                v
            })
            .collect();
        let out = json!({
            "schema": 1,
            "runtimeDir": path.to_string_lossy(),
            "ok": !failed,
            "survivesLogout": survives,
            "checks": list,
        });
        println!("{out:#}");
    } else {
        for c in &checks {
            println!("{:<5} {}: {}", c.status.name(), c.name, c.message);
        }
        match survives {
            Some(true) => {}
            Some(false) => println!(
                "\nSessions on this host are not expected to survive logout. See the warnings above."
            ),
            None => {
                println!("\nPerch could not tell whether sessions survive logout on this host.")
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Locks a scratch directory in the runtime directory through one open
/// description and checks that a second one sees the lock. On some network
/// file systems `flock` fails, or succeeds without locking anything.
fn flock_check(rt: &runtime::RuntimeDir) -> Check {
    let name = OsString::from(format!(".doctor-{}", runtime::random_name(8)));
    let result = (|| -> Result<bool, String> {
        sys::mkdir_at(rt.fd.as_fd(), &name, 0o700).map_err(|e| format!("mkdir: {e}"))?;
        let a = sys::open_dir_at(rt.fd.as_fd(), &name).map_err(|e| format!("open: {e}"))?;
        let b = sys::open_dir_at(rt.fd.as_fd(), &name).map_err(|e| format!("open: {e}"))?;
        if !sys::try_flock(a.as_fd(), Lock::Exclusive).map_err(|e| format!("flock: {e}"))? {
            return Err("flock: an exclusive lock on a new directory was refused".into());
        }
        Ok(!sys::try_flock(b.as_fd(), Lock::Shared).map_err(|e| format!("flock: {e}"))?)
    })();
    let _ = sys::unlink_at(rt.fd.as_fd(), &name, true);
    match result {
        Ok(true) => check(
            "flock",
            Status::Ok,
            "flock on a directory in the runtime directory works",
        ),
        Ok(false) => check(
            "flock",
            Status::Fail,
            "flock on a directory there does not lock: liveness cannot be told; set PERCH_RUNTIME_DIR to a local directory",
        ),
        Err(e) => check(
            "flock",
            Status::Fail,
            format!(
                "flock on a directory there failed ({e}); set PERCH_RUNTIME_DIR to a local directory"
            ),
        ),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::process::Command;

    use serde_json::json;

    use super::{Check, Status, check};
    use crate::sys;

    pub(super) fn checks(checks: &mut Vec<Check>, survives: &mut Option<bool>) {
        let user = user_name();
        let logind = logind(&user);
        let kill_applies = logind.kill_applies(&user);
        let linger = logind.linger;

        match std::fs::read_to_string("/proc/self/cgroup") {
            Ok(text) => {
                let path = cgroup_path(&text).unwrap_or_default();
                let mut c = match classify(&path) {
                    Cgroup::LoginScope => check(
                        "cgroup",
                        Status::Info,
                        format!(
                            "{path}: a login session's scope, which logind stops at logout if KillUserProcesses applies"
                        ),
                    ),
                    Cgroup::UserManager { service: None } => {
                        if linger == Some(true) {
                            check(
                                "cgroup",
                                Status::Info,
                                format!(
                                    "{path}: your service manager, which linger keeps running after logout"
                                ),
                            )
                        } else {
                            *survives = Some(false);
                            check(
                                "cgroup",
                                Status::Warn,
                                format!(
                                    "{path}: your service manager, which stops when your last login ends unless linger is on"
                                ),
                            )
                        }
                    }
                    Cgroup::UserManager {
                        service: Some(unit),
                    }
                    | Cgroup::Service(unit) => {
                        *survives = Some(false);
                        check(
                            "cgroup",
                            Status::Warn,
                            format!(
                                "{path}: started from the service {unit}; stopping or restarting it ends every session started from here"
                            ),
                        )
                    }
                    Cgroup::Other => check("cgroup", Status::Info, path.clone()),
                };
                c.data = json!({ "cgroup": path });
                checks.push(c);
            }
            Err(e) => checks.push(check(
                "cgroup",
                Status::Info,
                format!("cannot read /proc/self/cgroup: {e}"),
            )),
        }

        let mut c = match (&logind.kill, logind.source.as_str()) {
            (None, _) => {
                if *survives == Some(true) {
                    *survives = None;
                }
                check(
                    "logind",
                    Status::Info,
                    "cannot read logind's KillUserProcesses (no loginctl or logind.conf)",
                )
            }
            (Some(_), source) if kill_applies => {
                *survives = Some(false);
                check(
                    "logind",
                    Status::Warn,
                    format!(
                        "KillUserProcesses applies to {user} ({source}): logind ends every process of a login when it ends, sessions included"
                    ),
                )
            }
            (Some(k), source) => check(
                "logind",
                Status::Ok,
                format!("KillUserProcesses={k} does not apply to {user} ({source})"),
            ),
        };
        c.data = json!({
            "killUserProcesses": logind.kill,
            "killOnlyUsers": logind.only,
            "killExcludeUsers": logind.exclude,
            "linger": linger,
        });
        checks.push(c);

        if let Some(what) = container() {
            checks.push(check(
                "container",
                Status::Info,
                format!("running in a container ({what}): sessions end when it stops"),
            ));
        }
    }

    fn user_name() -> String {
        std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| sys::getuid().to_string())
    }

    /// The cgroup v2 path, or the systemd hierarchy's under v1.
    pub(super) fn cgroup_path(text: &str) -> Option<String> {
        let mut v1 = None;
        for line in text.lines() {
            let mut parts = line.splitn(3, ':');
            let (Some(id), Some(ctrl), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if id == "0" && ctrl.is_empty() {
                return Some(path.to_owned());
            }
            if ctrl == "name=systemd" {
                v1 = Some(path.to_owned());
            }
        }
        v1
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Cgroup {
        LoginScope,
        UserManager { service: Option<String> },
        Service(String),
        Other,
    }

    pub(super) fn classify(path: &str) -> Cgroup {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if let Some(i) = parts
            .iter()
            .position(|p| p.starts_with("user@") && p.ends_with(".service"))
        {
            let service = parts[i + 1..]
                .iter()
                .find(|p| p.ends_with(".service"))
                .map(|s| (*s).to_owned());
            return Cgroup::UserManager { service };
        }
        if parts
            .iter()
            .any(|p| p.starts_with("session-") && p.ends_with(".scope"))
        {
            return Cgroup::LoginScope;
        }
        match parts.iter().rev().find(|p| p.ends_with(".service")) {
            Some(s) => Cgroup::Service((*s).to_owned()),
            None => Cgroup::Other,
        }
    }

    #[derive(Debug, Default)]
    pub(super) struct Logind {
        pub kill: Option<String>,
        pub only: Vec<String>,
        pub exclude: Vec<String>,
        pub linger: Option<bool>,
        pub source: String,
    }

    impl Logind {
        /// Whether `KillUserProcesses` ends this user's processes.
        pub fn kill_applies(&self, user: &str) -> bool {
            let on = self.kill.as_deref().is_some_and(|k| {
                matches!(k.to_ascii_lowercase().as_str(), "yes" | "true" | "1" | "on")
            });
            // root is excluded by default.
            let exclude = if self.exclude.is_empty() && self.source.contains("conf") {
                vec!["root".to_owned()]
            } else {
                self.exclude.clone()
            };
            // As logind decides: off, excluded, or not among the only users.
            on && !exclude.iter().any(|u| u == user)
                && (self.only.is_empty() || self.only.iter().any(|u| u == user))
        }
    }

    fn loginctl() -> String {
        #[cfg(feature = "test-hooks")]
        if let Some(p) = std::env::var_os("PERCH_TEST_LOGINCTL") {
            return p.to_string_lossy().into_owned();
        }
        "loginctl".into()
    }

    /// `KEY=VALUE` lines.
    pub(super) fn properties(text: &str) -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    }

    fn words(v: Option<&String>) -> Vec<String> {
        v.map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// logind's settings: from `loginctl` if it answers, else from
    /// `logind.conf` and its drop-ins.
    fn logind(user: &str) -> Logind {
        let run = |args: &[&str]| {
            Command::new(loginctl())
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        // With no session named, `show-session` shows the manager.
        if let Some(text) = run(&[
            "show-session",
            "-p",
            "KillUserProcesses",
            "-p",
            "KillOnlyUsers",
            "-p",
            "KillExcludeUsers",
        ]) {
            let p = properties(&text);
            if let Some(kill) = p.get("KillUserProcesses") {
                let linger = run(&["show-user", user, "-p", "Linger"])
                    .and_then(|t| properties(&t).get("Linger").cloned())
                    .map(|l| l == "yes");
                return Logind {
                    kill: Some(kill.clone()),
                    only: words(p.get("KillOnlyUsers")),
                    exclude: words(p.get("KillExcludeUsers")),
                    linger,
                    source: "from loginctl".into(),
                };
            }
        }
        let mut conf = BTreeMap::new();
        let mut files = vec![std::path::PathBuf::from("/etc/systemd/logind.conf")];
        for d in [
            "/usr/lib/systemd/logind.conf.d",
            "/etc/systemd/logind.conf.d",
        ] {
            if let Ok(rd) = std::fs::read_dir(d) {
                let mut more: Vec<_> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "conf"))
                    .collect();
                more.sort();
                files.extend(more);
            }
        }
        for f in files {
            if let Ok(text) = std::fs::read_to_string(&f) {
                for line in text.lines() {
                    let line = line.trim();
                    if line.starts_with('#') || line.starts_with(';') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        conf.insert(k.trim().to_owned(), v.trim().to_owned());
                    }
                }
            }
        }
        Logind {
            kill: conf.get("KillUserProcesses").cloned(),
            only: words(conf.get("KillOnlyUsers")),
            exclude: words(conf.get("KillExcludeUsers")),
            linger: Some(
                std::path::Path::new("/var/lib/systemd/linger")
                    .join(user)
                    .exists(),
            )
            .filter(|_| std::path::Path::new("/var/lib/systemd/linger").is_dir()),
            source: "from logind.conf".into(),
        }
    }

    fn container() -> Option<&'static str> {
        if std::path::Path::new("/.dockerenv").exists() {
            return Some("docker");
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return Some("podman");
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cgroups() {
            assert_eq!(
                cgroup_path("12:cpu:/x\n1:name=systemd:/user.slice/a\n").as_deref(),
                Some("/user.slice/a")
            );
            assert_eq!(
                cgroup_path("0::/user.slice/user-1000.slice/session-3.scope\n").as_deref(),
                Some("/user.slice/user-1000.slice/session-3.scope")
            );
            assert_eq!(
                classify("/user.slice/user-1000.slice/session-3.scope"),
                Cgroup::LoginScope
            );
            assert_eq!(
                classify("/user.slice/user-1000.slice/user@1000.service/app.slice/app-x.scope"),
                Cgroup::UserManager { service: None }
            );
            assert_eq!(
                classify(
                    "/user.slice/user-1000.slice/user@1000.service/app.slice/code-tunnel.service"
                ),
                Cgroup::UserManager {
                    service: Some("code-tunnel.service".into())
                }
            );
            assert_eq!(
                classify("/system.slice/cron.service"),
                Cgroup::Service("cron.service".into())
            );
            assert_eq!(classify("/"), Cgroup::Other);
        }

        #[test]
        fn kill_user_processes() {
            let l = |kill: &str, only: &[&str], exclude: &[&str]| Logind {
                kill: Some(kill.into()),
                only: only.iter().map(|s| (*s).to_owned()).collect(),
                exclude: exclude.iter().map(|s| (*s).to_owned()).collect(),
                linger: None,
                source: "from loginctl".into(),
            };
            assert!(l("yes", &[], &[]).kill_applies("me"));
            assert!(!l("no", &[], &[]).kill_applies("me"));
            assert!(!l("yes", &[], &["me"]).kill_applies("me"));
            assert!(!l("no", &["me"], &[]).kill_applies("me"));
            assert!(l("yes", &["me"], &[]).kill_applies("me"));
            assert!(!l("yes", &["you"], &[]).kill_applies("me"));
            assert_eq!(
                properties("KillUserProcesses=yes\nKillOnlyUsers=\n")["KillUserProcesses"],
                "yes"
            );
        }
    }
}
