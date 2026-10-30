// SPDX-License-Identifier: GPL-3.0-only

//! `perch doctor`: the runtime directory, socket paths and `flock` on both
//! OSes; on Linux also logind's `KillUserProcesses`, read through a stand-in
//! `loginctl` (a test hook), and the cgroup.

mod common;

use common::Rt;
use serde_json::Value;

fn doctor(rt: &Rt, env: &[(&str, &str)]) -> (i32, Value) {
    let mut c = rt.cmd();
    c.args(["doctor", "--json"]);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    let v = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", common::describe(&out)));
    (out.status.code().unwrap(), v)
}

fn status<'a>(v: &'a Value, name: &str) -> &'a str {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check: {v:#}"))["status"]
        .as_str()
        .unwrap()
}

#[test]
fn a_good_runtime_directory_passes() {
    let rt = Rt::new();
    let (code, v) = doctor(&rt, &[]);
    assert_eq!(code, 0, "{v:#}");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["runtimeDir"], rt.path.to_str().unwrap());
    for name in ["runtime-dir", "socket-path", "flock"] {
        assert_eq!(status(&v, name), "ok", "{v:#}");
    }
    // It leaves nothing behind.
    assert_eq!(std::fs::read_dir(&rt.path).unwrap().count(), 0);
    // The text form says the same.
    let text = rt.ok(&["doctor"]);
    assert!(text.contains("ok    flock:"), "{text}");
}

#[test]
fn a_bad_runtime_directory_fails() {
    let rt = Rt::new();
    let bad = rt.path.join("open");
    std::fs::create_dir(&bad).unwrap();
    perch::sys::fchmod(
        std::os::fd::AsFd::as_fd(&perch::sys::open_dir(&bad).unwrap()),
        0o755,
    )
    .unwrap();
    let (code, v) = doctor(&rt, &[("PERCH_RUNTIME_DIR", bad.to_str().unwrap())]);
    assert_eq!(code, 1);
    assert_eq!(v["ok"], false);
    assert_eq!(status(&v, "runtime-dir"), "fail");
    assert!(
        v["checks"][0]["message"].as_str().unwrap().contains("0755"),
        "{v:#}"
    );
    // A path too long for a socket.
    let long = rt.path.join("d".repeat(100));
    let (code, v) = doctor(&rt, &[("PERCH_RUNTIME_DIR", long.to_str().unwrap())]);
    assert_eq!(code, 1);
    assert_eq!(status(&v, "socket-path"), "fail");
}

#[cfg(all(target_os = "linux", feature = "test-hooks"))]
#[test]
fn kill_user_processes_is_a_warning_on_linux() {
    use std::os::unix::fs::PermissionsExt;
    let rt = Rt::new();
    let fake = rt.path.join("loginctl");
    let write = |kill: &str| {
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  show-session) printf 'KillUserProcesses={kill}\\nKillOnlyUsers=\\nKillExcludeUsers=root\\n';;\n  show-user) echo Linger=yes;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    write("yes");
    let env = [
        ("PERCH_TEST_LOGINCTL", fake.to_str().unwrap()),
        ("USER", "someone"),
    ];
    let (code, v) = doctor(&rt, &env);
    assert_eq!(code, 0, "a warning is not a failure: {v:#}");
    assert_eq!(status(&v, "logind"), "warn", "{v:#}");
    assert_eq!(v["survivesLogout"], false);
    let logind = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "logind")
        .unwrap();
    assert_eq!(logind["killUserProcesses"], "yes");
    assert_eq!(logind["linger"], true);
    write("no");
    let (_, v) = doctor(&rt, &env);
    assert_eq!(status(&v, "logind"), "ok", "{v:#}");
    assert!(status(&v, "cgroup") != "fail");
}
