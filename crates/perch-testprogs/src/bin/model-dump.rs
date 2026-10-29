// SPDX-License-Identifier: GPL-3.0-only

//! `model-dump SCRIPT OUTDIR` runs a fidelity fixture's action script
//! through the gate (the tracker and the hold-back rules) and `perch-vt`,
//! as the supervisor and the model process will, and writes what the
//! harness in `tests/fidelity` checks:
//!
//! - `restore-N.bin`: the bytes a client receives for the Nth restore:
//!   the restore, then the bytes the gate was still holding back;
//! - `hostile.bin`: the client state every restore is also tried against
//!   (alternate screen, margins, insert mode, an open OSC 8 and a
//!   half-written CSI);
//! - `summary.json`: for each restore, the model's state when it was built
//!   (`expected`, as `perch_vt_dump` sees it) and the differences found by
//!   feeding it to a second `perch-vt`, plain and hostile, right after the
//!   restore and again after the actions that follow it.
//!
//! `model-dump --memory LINES COLS` instead fills a model with LINES lines
//! of history at COLS columns and prints its peak resident memory.
//!
//! The script is JSON:
//!
//! ```json
//! { "size": [80, 24], "scrollback": 1000, "stream": "stream.bin",
//!   "actions": [ {"feed": 120}, {"feed": null}, {"resize": [100, 30]},
//!     {"restore": {"mode": "full", "size": [80, 24], "scrollback": 1000}},
//!     {"exit_alt": {"mode": 1049, "then": "more\r\n"}} ] }
//! ```
//!
//! `feed` takes the next N bytes of the stream (all of the rest for null);
//! `exit_alt` is the program's own exit from the alternate screen followed
//! by more output. A restore may set `byte_cap` to test the fallback.

use perch::restore::{self, Client, Gate, Mode};
use perch::vt_ffi::{Model, RestoreLimits};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The client state every restore is also restored into.
pub const HOSTILE: &[u8] = b"\x1b[?1049hhostile\x1b[2;5r\x1b[?69h\x1b[3;9s\x1b[4h\x1b]8;;http://hostile.invalid/\x1b\\link\x1b[38;5";

/// History limits for models: the scrollback byte limit Perch sets.
const SCROLLBACK_BYTES: u64 = 64 << 20;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, lines, cols] if flag == "--memory" => {
            memory(lines.parse().expect("LINES"), cols.parse().expect("COLS"))
        }
        [script, out] => run(Path::new(script), Path::new(out)),
        _ => {
            eprintln!("usage: model-dump SCRIPT OUTDIR | model-dump --memory LINES COLS");
            std::process::exit(2);
        }
    }
}

/// A second terminal fed what a client receives, compared with the model.
struct Replay {
    plain: Model,
    hostile: Model,
    index: usize,
    mode: Mode,
}

fn size(v: &Value) -> (u16, u16) {
    let n = |i: usize| v[i].as_u64().expect("size is [cols, rows]") as u16;
    (n(0), n(1))
}

fn dump(m: &Model) -> Value {
    serde_json::from_slice(&m.dump().expect("dump")).expect("dump is JSON")
}

fn run(script_path: &Path, out: &Path) {
    let script: Value = serde_json::from_slice(
        &std::fs::read(script_path).unwrap_or_else(|e| panic!("{}: {e}", script_path.display())),
    )
    .expect("script is JSON");
    let dir = script_path.parent().unwrap_or(Path::new("."));
    let stream_path: PathBuf = dir.join(script["stream"].as_str().expect("stream"));
    let stream =
        std::fs::read(&stream_path).unwrap_or_else(|e| panic!("{}: {e}", stream_path.display()));
    let (cols, rows) = size(&script["size"]);
    let scrollback = script["scrollback"].as_u64().unwrap_or(1000) as u32;
    std::fs::create_dir_all(out).expect("create OUTDIR");
    std::fs::write(out.join("hostile.bin"), HOSTILE).expect("write hostile.bin");

    let mut model = Model::new(cols, rows, scrollback, SCROLLBACK_BYTES).expect("model");
    let mut gate = Gate::new();
    let mut frame = 0u64;
    let mut pos = 0usize;
    let mut replay: Option<Replay> = None;
    let mut restores: Vec<Value> = Vec::new();

    // Feeds output as the supervisor would: through the gate to the model,
    // and raw to the terminals that were restored.
    let mut feed =
        |bytes: &[u8], model: &mut Model, gate: &mut Gate, replay: &mut Option<Replay>| {
            for chunk in gate.push(bytes).chunks {
                frame += 1;
                model.feed(&chunk.bytes, frame);
            }
            if let Some(r) = replay {
                r.plain.feed(bytes, 0);
                r.hostile.feed(bytes, 0);
            }
        };

    for action in script["actions"].as_array().expect("actions") {
        if let Some(n) = action.get("feed") {
            let end = match n.as_u64() {
                Some(n) => (pos + n as usize).min(stream.len()),
                None => stream.len(),
            };
            feed(&stream[pos..end], &mut model, &mut gate, &mut replay);
            pos = end;
        } else if let Some(s) = action.get("resize") {
            let (c, r) = size(s);
            model.resize(c, r).expect("resize");
            if let Some(rp) = &mut replay {
                rp.plain.resize(c, r).expect("resize");
                rp.hostile.resize(c, r).expect("resize");
            }
        } else if let Some(e) = action.get("exit_alt") {
            let mode = e["mode"].as_u64().expect("exit_alt mode");
            let mut bytes = format!("\x1b[?{mode}l").into_bytes();
            bytes.extend_from_slice(e["then"].as_str().unwrap_or("").as_bytes());
            feed(&bytes, &mut model, &mut gate, &mut replay);
        } else if let Some(r) = action.get("restore") {
            finish(&model, replay.take(), &mut restores);
            let index = restores.len();
            let mode = match r["mode"].as_str() {
                Some("screen") => Mode::Screen,
                _ => Mode::Full,
            };
            let (c, rw) = size(&r["size"]);
            let client = Client {
                cols: c,
                rows: rw,
                scrollback_rows: r["scrollback"].as_u64().unwrap_or(1000) as u32,
            };
            let paste = gate.tracker().bracketed_paste();
            let built = if gate.partial() {
                restore::partial(client, paste)
            } else {
                let limits = RestoreLimits {
                    client_scrollback_rows: client.scrollback_rows,
                    history_cap: r["history_cap"]
                        .as_u64()
                        .map_or(restore::HISTORY_CAP, |n| n as usize),
                    byte_cap: r["byte_cap"]
                        .as_u64()
                        .map_or(restore::BYTE_CAP, |n| n as usize),
                };
                restore::build_with_limits(&mut model, mode, client, paste, limits)
                    .expect("restore")
            };
            let mut client_bytes = built.bytes.clone();
            client_bytes.extend_from_slice(gate.held());
            std::fs::write(out.join(format!("restore-{index}.bin")), &client_bytes)
                .expect("write restore");

            let expected = dump(&model);
            let (rc, rr) = built.size;
            let new_client =
                || Model::new(rc, rr, client.scrollback_rows, SCROLLBACK_BYTES).expect("model");
            let mut plain = new_client();
            let mut hostile = new_client();
            hostile.feed(HOSTILE, 0);
            plain.feed(&built.bytes, 0);
            hostile.feed(&built.bytes, 0);
            let compared = built.nudge.is_none();
            let diff = |m: &Model| {
                if compared {
                    restore::differences(&expected, &dump(m), mode)
                } else {
                    Vec::new()
                }
            };
            restores.push(json!({
                "index": index,
                "mode": if mode == Mode::Full { "full" } else { "screen" },
                "size": [rc, rr],
                "scrollback": client.scrollback_rows,
                "held": gate.held().len(),
                "partial": built.nudge.is_some(),
                "nudge": built.nudge.map(|n| json!({"via": [n.via.0, n.via.1], "back": [n.back.0, n.back.1]})),
                "prologue_len": restore::prologue(rc).len(),
                "notice": String::from_utf8_lossy(restore::NOTICE),
                "expected": expected,
                "differences": diff(&plain),
                "differences_hostile": diff(&hostile),
            }));
            // The client then gets the held bytes as live output.
            plain.feed(gate.held(), 0);
            hostile.feed(gate.held(), 0);
            replay = compared.then_some(Replay {
                plain,
                hostile,
                index,
                mode,
            });
        } else {
            panic!("unknown action {action}");
        }
    }
    finish(&model, replay.take(), &mut restores);
    let summary = json!({ "restores": restores, "final": dump(&model) });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary).expect("summary"),
    )
    .expect("write summary");
}

/// The comparison after the actions that followed a restore.
fn finish(model: &Model, replay: Option<Replay>, restores: &mut [Value]) {
    let Some(r) = replay else { return };
    let expected = dump(model);
    let entry = &mut restores[r.index];
    entry["differences_after"] = json!(restore::differences(&expected, &dump(&r.plain), r.mode));
    entry["differences_after_hostile"] =
        json!(restore::differences(&expected, &dump(&r.hostile), r.mode));
}

/// Fill a model with `lines` lines of history at `cols` columns, every
/// cell printed and a color change every 8 cells, and print its peak
/// resident memory.
fn memory(lines: u32, cols: u16) {
    let before = perch::sys::max_rss_bytes();
    let rows = 50;
    let mut model = Model::new(cols, rows, lines, SCROLLBACK_BYTES).expect("model");
    let mut line = Vec::new();
    for i in 0..(lines as usize + rows as usize) {
        line.clear();
        for col in 0..cols as usize {
            if col % 8 == 0 {
                line.extend_from_slice(format!("\x1b[38;5;{}m", (i + col) % 256).as_bytes());
            }
            line.push(b'a' + ((i + col) % 26) as u8);
        }
        line.extend_from_slice(b"\x1b[0m\r\n");
        model.feed(&line, 0);
    }
    // Before the dump, which is far larger than the model.
    let after = perch::sys::max_rss_bytes();
    let history = serde_json::from_slice::<Value>(&model.dump().expect("dump")).expect("JSON")
        ["screens"]["primary"]["history_rows"]
        .as_u64()
        .unwrap_or(0);
    println!(
        "{}",
        json!({"lines": lines, "cols": cols, "history_rows": history,
               "max_rss_before": before, "max_rss": after})
    );
}
