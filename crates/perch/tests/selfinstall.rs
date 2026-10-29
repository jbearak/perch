// SPDX-License-Identifier: GPL-3.0-only

//! `perch self-install`: concurrent installers of different versions, the
//! newest winning under the lock, and the fallback without `flock`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::{describe, perch_bin};
use serde_json::Value;

/// A scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let p = std::env::temp_dir().join(format!("perch-si-{}", perch::runtime::random_name(8)));
        std::fs::create_dir_all(&p).expect("create scratch directory");
        Scratch(p)
    }

    /// The install directory, not yet created.
    fn dest(&self) -> PathBuf {
        self.0.join("bin").join("os-arch")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn install_cmd(dest: &Path, env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(perch_bin());
    c.arg("self-install")
        .arg(dest)
        .env_remove("PERCH_TEST_VERSION")
        .env_remove("PERCH_TEST_NO_FLOCK")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn report(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "self-install failed: {}",
        describe(out)
    );
    serde_json::from_slice(&out.stdout).expect("self-install prints JSON")
}

/// The installed copy is a whole, executable copy of the binary under test,
/// and no temporary file is left.
fn assert_intact(dest: &Path) {
    let installed = dest.join("perch");
    let bytes = std::fs::read(&installed).expect("read the copy");
    assert!(
        bytes == std::fs::read(perch_bin()).expect("read perch"),
        "the copy differs from the binary"
    );
    let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    let names: Vec<String> = std::fs::read_dir(dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(".tmp-")),
        "temporary files left: {names:?}"
    );
    let out = Command::new(&installed)
        .arg("version")
        .output()
        .expect("run the copy");
    assert!(out.status.success(), "the copy runs: {}", describe(&out));
}

fn recorded_version(dest: &Path) -> String {
    std::fs::read_to_string(dest.join("perch.version"))
        .expect("read perch.version")
        .trim()
        .to_owned()
}

#[test]
fn installs_then_keeps_the_same_version() {
    let s = Scratch::new();
    let dest = s.dest();
    let first = report(&install_cmd(&dest, &[]).output().unwrap());
    assert_eq!(first["installed"], true);
    assert_eq!(first["locked"], true);
    assert_eq!(first["previous"], Value::Null);
    assert_eq!(first["path"], dest.join("perch").to_str().unwrap());
    assert_intact(&dest);
    assert_eq!(recorded_version(&dest), perch::VERSION);

    let again = report(&install_cmd(&dest, &[]).output().unwrap());
    assert_eq!(again["installed"], false);
    assert_eq!(again["previous"], perch::VERSION);
}

#[test]
fn replaces_a_missing_or_damaged_copy() {
    let s = Scratch::new();
    let dest = s.dest();
    report(&install_cmd(&dest, &[]).output().unwrap());
    std::fs::remove_file(dest.join("perch")).unwrap();
    let r = report(&install_cmd(&dest, &[]).output().unwrap());
    assert_eq!(r["installed"], true);
    assert_intact(&dest);

    std::fs::write(dest.join("perch.version"), "garbage\n").unwrap();
    let r = report(&install_cmd(&dest, &[]).output().unwrap());
    assert_eq!(r["installed"], true);
    assert_eq!(recorded_version(&dest), perch::VERSION);
}

#[test]
fn refuses_a_relative_directory() {
    let out = Command::new(perch_bin())
        .args(["self-install", "relative/dir"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
}

/// Twenty installers at once, all of the same version, leave one whole copy.
#[test]
fn concurrent_installers_leave_one_whole_copy() {
    let s = Scratch::new();
    let dest = s.dest();
    let children: Vec<_> = (0..20)
        .map(|_| install_cmd(&dest, &[]).spawn().unwrap())
        .collect();
    let reports: Vec<Value> = children
        .into_iter()
        .map(|c| report(&c.wait_with_output().unwrap()))
        .collect();
    let installs = reports.iter().filter(|r| r["installed"] == true).count();
    assert_eq!(installs, 1, "exactly one installer copies: {reports:?}");
    assert_intact(&dest);
}

#[cfg(feature = "test-hooks")]
mod hooks {
    use super::*;

    #[test]
    fn newer_wins_and_older_does_not_replace_it() {
        let s = Scratch::new();
        let dest = s.dest();
        let v = |ver: &str| {
            report(
                &install_cmd(&dest, &[("PERCH_TEST_VERSION", ver)])
                    .output()
                    .unwrap(),
            )
        };
        assert_eq!(v("0.2.0")["installed"], true);
        let older = v("0.1.9");
        assert_eq!(older["installed"], false);
        assert_eq!(older["previous"], "0.2.0");
        assert_eq!(recorded_version(&dest), "0.2.0");
        assert_eq!(v("0.2.0-rc.1")["installed"], false);
        assert_eq!(v("0.10.0")["installed"], true);
        assert_eq!(recorded_version(&dest), "0.10.0");
        assert_intact(&dest);
    }

    /// Installers of many versions racing: whatever the order, the newest
    /// is what remains.
    #[test]
    fn concurrent_installers_of_different_versions_leave_the_newest() {
        let s = Scratch::new();
        let dest = s.dest();
        let versions: Vec<String> = (0..16).map(|i| format!("0.{}.{}", i % 5, i)).collect();
        let children: Vec<_> = versions
            .iter()
            .map(|v| {
                install_cmd(&dest, &[("PERCH_TEST_VERSION", v)])
                    .spawn()
                    .unwrap()
            })
            .collect();
        for c in children {
            let r = report(&c.wait_with_output().unwrap());
            assert_eq!(r["locked"], true);
        }
        assert_eq!(recorded_version(&dest), "0.4.14");
        assert_intact(&dest);
    }

    /// Without `flock`, every installer writes its own version, but the copy
    /// is never torn and no temporary file is left.
    #[test]
    fn without_flock_every_installer_installs_and_nothing_is_torn() {
        let s = Scratch::new();
        let dest = s.dest();
        report(
            &install_cmd(&dest, &[("PERCH_TEST_VERSION", "0.9.0")])
                .output()
                .unwrap(),
        );
        let versions: Vec<String> = (0..16).map(|i| format!("0.1.{i}")).collect();
        let children: Vec<_> = versions
            .iter()
            .map(|v| {
                install_cmd(
                    &dest,
                    &[("PERCH_TEST_VERSION", v), ("PERCH_TEST_NO_FLOCK", "1")],
                )
                .spawn()
                .unwrap()
            })
            .collect();
        for c in children {
            let r = report(&c.wait_with_output().unwrap());
            assert_eq!(r["locked"], false);
            assert_eq!(
                r["installed"], true,
                "an older version installs without the lock"
            );
        }
        assert!(versions.contains(&recorded_version(&dest)));
        assert_intact(&dest);
    }
}
