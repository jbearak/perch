// SPDX-License-Identifier: GPL-3.0-only

use std::process::Command;

fn perch(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perch"))
        .args(args)
        .output()
        .expect("run perch")
}

#[test]
fn version_json_reports_a_passing_selftest() {
    let out = perch(&["version", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report["ghostty_commit"],
        "521cf2c5d30bd48028ca61546c64e7913764073d"
    );
    let selftest = &report["selftest"];
    assert_eq!(selftest["ok"], true, "{report:#}");
    assert_eq!(selftest["dsr6_reply"], "\u{1b}[2;1R");
    assert_eq!(selftest["cursor"]["row"], 1);
    assert_eq!(selftest["kitty_graphics"], false);
}

#[test]
fn version_text() {
    let out = perch(&["version"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("perch "), "{text}");
    assert!(text.contains("Ghostty 521cf2c"), "{text}");
}

#[test]
fn unknown_command_is_a_usage_error() {
    let out = perch(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
}
