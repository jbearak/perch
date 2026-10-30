// SPDX-License-Identifier: GPL-3.0-only

//! The fidelity harness end to end. For each fixture below and each restore
//! mode, a real session's program writes the fixture's output up to its
//! first restore, a recording client attaches and records what the
//! supervisor sends it (the restore and the held-back bytes), and the
//! program then writes what follows (recorded as live output).
//!
//! The recordings go to `$PERCH_E2E_RECORDINGS` if set, and
//! `node tests/fidelity/run.mjs --recorded <dir>` checks the screens in
//! headless xterm.js, as for model-dump's restores. CI runs both. Without
//! the variable, the recordings are checked here only for their shape.

#![cfg(feature = "test-hooks")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::term::Recorder;
use common::{Rt, testprog, wait_for};
use perch::proto::kind;
use perch::restore;
use serde_json::{Value, json};

/// Fixtures whose first restore is at the size they were written at.
const FIXTURES: &[&str] = &[
    "alt-1049-1049",
    "alt-47-1047",
    "dec-special-margin",
    "decstr",
    "fish-right-prompt",
    "history-blank-tail",
    "history-client-limit",
    "hyperlinks",
    "less",
    "mode-2027",
    "partial-sequences",
    "progress-bar",
    "r-prompt",
    "reedline-redraw",
    "split-utf8",
    "vim-in-out",
    "wide-graphemes",
    "zsh-right-prompt",
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fidelity/fixtures")
}

/// The bytes of one action that writes output, or `None` for another.
fn action_bytes(a: &Value, stream: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    if let Some(n) = a.get("feed") {
        let end = match n.as_u64() {
            Some(n) => (*pos + n as usize).min(stream.len()),
            None => stream.len(),
        };
        let b = stream[*pos..end].to_vec();
        *pos = end;
        return Some(b);
    }
    if let Some(f) = a.get("fill") {
        let byte = f["byte"].as_u64().unwrap() as u8;
        return Some(vec![byte; f["count"].as_u64().unwrap() as usize]);
    }
    if let Some(r) = a.get("repeat") {
        let text = r["text"].as_str().unwrap();
        return Some(
            text.repeat(r["count"].as_u64().unwrap() as usize)
                .into_bytes(),
        );
    }
    if let Some(e) = a.get("exit_alt") {
        let then = e["then"].as_str().unwrap_or("");
        return Some(format!("\x1b[?{}l{then}", e["mode"]).into_bytes());
    }
    None
}

/// A fixture's output before its first restore, the restore, and the
/// output after it up to the next action that is not output.
fn split(script: &Value, stream: &[u8]) -> (Vec<u8>, Value, Vec<u8>) {
    let actions = script["actions"].as_array().unwrap();
    let mut pos = 0;
    let mut before = Vec::new();
    let mut i = 0;
    while let Some(b) = action_bytes(&actions[i], stream, &mut pos) {
        before.extend(b);
        i += 1;
    }
    let restore = actions[i]["restore"].clone();
    assert!(
        !restore.is_null(),
        "the first action after output is a restore"
    );
    let mut after = Vec::new();
    for a in &actions[i + 1..] {
        match action_bytes(a, stream, &mut pos) {
            Some(b) => after.extend(b),
            None => break,
        }
    }
    (before, restore, after)
}

/// Runs `words` in the session's dsr-timer and waits for the round.
fn round(rt: &Rt, id: &str, n: usize, words: &str) {
    let go = rt.dir(id).join("go");
    std::fs::write(go.with_extension("tmp"), words).unwrap();
    std::fs::rename(go.with_extension("tmp"), &go).unwrap();
    wait_for(Duration::from_secs(30), || {
        let text = std::fs::read_to_string(rt.dir(id).join("out")).ok()?;
        (text.lines().count() >= n).then_some(())
    })
    .unwrap_or_else(|| panic!("round `{words}` never finished"));
}

fn record(rt: &Rt, name: &str, mode: &str, out: &Path) {
    let dir = fixtures_dir().join(name);
    let script: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("script.json")).unwrap()).unwrap();
    let stream = std::fs::read(dir.join(script["stream"].as_str().unwrap())).unwrap();
    let (before, restore_spec, after) = split(&script, &stream);
    let size = &script["size"];
    let (cols, rows) = (size[0].as_u64().unwrap(), size[1].as_u64().unwrap());
    let scrollback = script["scrollback"].as_u64().unwrap_or(1000);
    let client_scrollback = restore_spec["scrollback"].as_u64().unwrap_or(1000);
    assert_eq!(restore_spec["size"], json!([cols, rows]), "{name}");

    let rec = out.join(format!("{name}-{mode}"));
    std::fs::create_dir_all(&rec).unwrap();
    std::fs::write(rec.join("program-0.bin"), &before).unwrap();
    std::fs::write(rec.join("program-1.bin"), &after).unwrap();

    let prog = testprog("dsr-timer");
    let id = rt.new_session(&[
        "--scrollback",
        &scrollback.to_string(),
        "--",
        prog.to_str().unwrap(),
        "{dir}/out",
        "{dir}/go",
    ]);
    // The session takes the fixture's size from a client that restores
    // nothing.
    let (sizer, answer) = Recorder::attach(
        rt,
        &id,
        json!({"restore": "none", "cols": cols, "rows": rows}),
    );
    assert_eq!(answer.kind, kind::ATTACHED);
    sizer.close();
    let p0 = rec.join("program-0.bin");
    round(
        rt,
        &id,
        1,
        &format!("cat:{}:0:{}", p0.display(), before.len()),
    );
    let (mut r, answer) = Recorder::attach(
        rt,
        &id,
        json!({"restore": mode, "cols": cols, "rows": rows, "scrollback": client_scrollback}),
    );
    assert_eq!(answer.kind, kind::ATTACHED);
    r.until(Duration::from_secs(10), |r| !r.output.is_empty());
    r.settle(Duration::from_millis(400));
    let restored = std::mem::take(&mut r.output);
    assert!(
        restored.starts_with(&restore::prologue(cols as u16)),
        "{name} {mode}: a restore starts with the prologue"
    );
    let p1 = rec.join("program-1.bin");
    round(
        rt,
        &id,
        2,
        &format!("cat:{}:0:{}", p1.display(), after.len()),
    );
    r.settle(Duration::from_millis(400));
    assert_eq!(
        r.output, after,
        "{name} {mode}: live output is passed through"
    );
    std::fs::write(rec.join("restore.bin"), &restored).unwrap();
    std::fs::write(rec.join("live.bin"), &r.output).unwrap();
    let meta = json!({
        "fixture": name,
        "mode": mode,
        "size": [cols, rows],
        "scrollback": scrollback,
        "client_scrollback": client_scrollback,
    });
    std::fs::write(rec.join("recording.json"), format!("{meta:#}\n")).unwrap();
    rt.ok(&["kill", &id]);
}

#[test]
fn record_restores_from_a_real_supervisor() {
    let out = match std::env::var_os("PERCH_E2E_RECORDINGS") {
        Some(p) => PathBuf::from(p),
        None => std::env::temp_dir().join(format!("perch-e2e-{}", perch::runtime::random_name(8))),
    };
    std::fs::create_dir_all(&out).unwrap();
    let rt = Rt::new();
    for name in FIXTURES {
        for mode in ["full", "screen"] {
            record(&rt, name, mode, &out);
        }
    }
    if std::env::var_os("PERCH_E2E_RECORDINGS").is_none() {
        let _ = std::fs::remove_dir_all(&out);
    }
}
