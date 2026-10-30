// SPDX-License-Identifier: GPL-3.0-only

//! `model-dump SCRIPT OUTDIR` runs a fidelity fixture's action script
//! through the supervisor's `Feeder` (the tracker, the hold-back rule and
//! the 16 MiB rule) into `perch-vt`, as the supervisor does, and writes
//! what the
//! harness in `tests/fidelity` checks:
//!
//! - `restore-N.bin`: the bytes a client receives for the Nth restore:
//!   the restore, then the bytes the feeder was still holding back;
//! - `live-N.bin`: the live output that client received after it;
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
//! `fill` feeds `count` copies of `byte` and `repeat` feeds `text` (UTF-8)
//! `count` times, for output too long to commit;
//! `exit_alt` is the program's own exit from the alternate screen followed
//! by more output. A restore may set `byte_cap` or `history_cap`.
//!
//! Each restore opens a window that ends at the next restore, resize or
//! the end of the script. `live-N.bin` holds the output the client
//! received in it: everything after the restore, less the rest of a
//! sequence over 16 MiB that was arriving when it attached, which the
//! supervisor withholds.

use perch::model::feed::{Feeder, Step};
use perch::restore::{self, Client, Mode};
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

/// The terminals restored by the latest restore, and what they received
/// since: a second `perch-vt` per client state, compared with the model.
struct Window {
    plain: Model,
    hostile: Model,
    index: usize,
    mode: Mode,
    /// Whether the restore was built from the model (not the partial one).
    compared: bool,
    /// Live output the client received after the restore.
    live: Vec<u8>,
    /// The restore came in the middle of a long sequence: the rest of it is
    /// withheld from this client.
    withholding: bool,
}

struct Run {
    model: Model,
    feeder: Feeder,
    /// Set by a long sequence fed through, cleared by RIS, as the
    /// supervisor does.
    partial: bool,
    /// How many long sequences the feeder dropped and fed through.
    discarded: u64,
    fed_through: u64,
    window: Option<Window>,
    restores: Vec<Value>,
    out: PathBuf,
}

fn size(v: &Value) -> (u16, u16) {
    let n = |i: usize| v[i].as_u64().expect("size is [cols, rows]") as u16;
    (n(0), n(1))
}

fn dump(m: &Model) -> Value {
    serde_json::from_slice(&m.dump().expect("dump")).expect("dump is JSON")
}

impl Run {
    /// Output as the supervisor handles it: through the feeder to the
    /// model, and live to the restored client, less what it withholds.
    fn feed(&mut self, mut bytes: &[u8]) {
        if self.window.as_ref().is_some_and(|w| w.withholding) {
            let mut n = 0;
            while n < bytes.len() && self.feeder.in_long_sequence() {
                self.push(&bytes[n..n + 1]);
                n += 1;
            }
            bytes = &bytes[n..];
            if self.feeder.in_long_sequence() {
                return;
            }
            // The long sequence is over. What the feeder holds now (the ESC
            // of an ST, say) starts what follows it, so the client gets it.
            let started = self.feeder.held().to_vec();
            if let Some(w) = &mut self.window {
                w.withholding = false;
                w.live.extend_from_slice(&started);
                w.plain.feed(&started, 0);
                w.hostile.feed(&started, 0);
            }
        }
        self.push(bytes);
        if let Some(w) = &mut self.window {
            w.live.extend_from_slice(bytes);
            w.plain.feed(bytes, 0);
            w.hostile.feed(bytes, 0);
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for step in self.feeder.push(bytes) {
            match step {
                Step::Frame { n, bytes } => self.model.feed(&bytes, n),
                Step::Partial => {
                    self.partial = true;
                    self.fed_through += 1;
                }
                Step::Reset => self.partial = false,
                Step::Discarded => self.discarded += 1,
                Step::Query { .. } | Step::LongEnd { .. } => {}
            }
        }
    }

    fn restore(&mut self, r: &Value) {
        self.finish();
        let index = self.restores.len();
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
        let paste = self.feeder.tracker().bracketed_paste();
        let withholding = self.feeder.in_long_sequence();
        let history_cap = r["history_cap"]
            .as_u64()
            .map_or(restore::HISTORY_CAP, |n| n as usize);
        let built = if self.partial {
            restore::partial(client, paste)
        } else {
            let limits = RestoreLimits {
                client_scrollback_rows: client.scrollback_rows,
                history_cap,
                byte_cap: r["byte_cap"]
                    .as_u64()
                    .map_or(restore::BYTE_CAP, |n| n as usize),
            };
            restore::build_with_limits(&mut self.model, mode, client, paste, limits)
                .expect("restore")
        };
        let held = self.feeder.held().to_vec();
        let mut client_bytes = built.bytes.clone();
        client_bytes.extend_from_slice(&held);
        std::fs::write(self.out.join(format!("restore-{index}.bin")), &client_bytes)
            .expect("write restore");

        let mut expected = dump(&self.model);
        // The history rows the restore sends, from the client's scrollback
        // and the history cap: the oracles check the client kept exactly
        // these.
        let history_sent = if mode == Mode::Full && built.nudge.is_none() {
            self.model
                .history_kept(client.scrollback_rows, history_cap)
                .expect("history kept")
        } else {
            0
        };
        expected["history_sent"] = json!(history_sent);
        let (rc, rr) = built.size;
        // More scrollback than the client keeps, so this terminal's own
        // pruning (by whole pages) cannot hide rows the restore left out.
        let new_client = || {
            Model::new(
                rc,
                rr,
                client.scrollback_rows.saturating_mul(2).max(10_000),
                SCROLLBACK_BYTES,
            )
            .expect("model")
        };
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
        self.restores.push(json!({
            "index": index,
            "mode": if mode == Mode::Full { "full" } else { "screen" },
            "size": [rc, rr],
            "scrollback": client.scrollback_rows,
            "history_sent": history_sent,
            "held": held.len(),
            "withheld": withholding,
            "partial": built.nudge.is_some(),
            "nudge": built.nudge.map(|n| json!({"via": [n.via.0, n.via.1], "back": [n.back.0, n.back.1]})),
            "prologue_len": restore::prologue(rc).len(),
            "notice": String::from_utf8_lossy(restore::NOTICE),
            "expected": expected,
            "differences": diff(&plain),
            "differences_hostile": diff(&hostile),
        }));
        // The client then gets the held bytes as live output.
        plain.feed(&held, 0);
        hostile.feed(&held, 0);
        self.window = Some(Window {
            plain,
            hostile,
            index,
            mode,
            compared,
            live: Vec::new(),
            withholding,
        });
    }

    /// Ends the latest restore's window: writes its live output and
    /// compares again.
    fn finish(&mut self) {
        let Some(w) = self.window.take() else { return };
        std::fs::write(self.out.join(format!("live-{}.bin", w.index)), &w.live)
            .expect("write live");
        let entry = &mut self.restores[w.index];
        entry["live"] = json!(w.live.len());
        let mut after = dump(&self.model);
        // The rows the restore sent, plus those the output scrolled up, less
        // any it cleared: the newest that fit the client's scrollback.
        // Counted on the restored terminal, which keeps more than the
        // client and so prunes none of them, and held exactly
        // `history_sent` before the output; the model prunes by whole
        // pages, so its own count cannot say.
        let plain = dump(&w.plain);
        let rows = |v: &Value| {
            v["screens"]["primary"]["history_rows"]
                .as_u64()
                .unwrap_or(0)
        };
        let limit = entry["scrollback"].as_u64().unwrap_or(0);
        after["history_sent"] = json!(rows(&plain).min(limit));
        after["history_exact"] = json!(false);
        entry["after"] = after;
        if w.compared {
            let expected = &entry["after"];
            let plain = restore::differences(expected, &plain, w.mode);
            let hostile = restore::differences(expected, &dump(&w.hostile), w.mode);
            entry["differences_after"] = json!(plain);
            entry["differences_after_hostile"] = json!(hostile);
        }
    }
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

    let mut run = Run {
        model: Model::new(cols, rows, scrollback, SCROLLBACK_BYTES).expect("model"),
        feeder: Feeder::new(),
        partial: false,
        discarded: 0,
        fed_through: 0,
        window: None,
        restores: Vec::new(),
        out: out.to_path_buf(),
    };
    let mut pos = 0usize;
    for action in script["actions"].as_array().expect("actions") {
        if let Some(n) = action.get("feed") {
            let end = match n.as_u64() {
                Some(n) => (pos + n as usize).min(stream.len()),
                None => stream.len(),
            };
            run.feed(&stream[pos..end]);
            pos = end;
        } else if let Some(f) = action.get("fill") {
            let byte = f["byte"].as_u64().expect("fill byte") as u8;
            let count = f["count"].as_u64().expect("fill count") as usize;
            run.feed(&vec![byte; count]);
        } else if let Some(r) = action.get("repeat") {
            let text = r["text"].as_str().expect("repeat text").as_bytes();
            let count = r["count"].as_u64().expect("repeat count") as usize;
            run.feed(&text.repeat(count));
        } else if let Some(s) = action.get("resize") {
            run.finish();
            let (c, r) = size(s);
            run.model.resize(c, r).expect("resize");
        } else if let Some(e) = action.get("exit_alt") {
            let mode = e["mode"].as_u64().expect("exit_alt mode");
            let mut bytes = format!("\x1b[?{mode}l").into_bytes();
            bytes.extend_from_slice(e["then"].as_str().unwrap_or("").as_bytes());
            run.feed(&bytes);
        } else if let Some(r) = action.get("restore") {
            run.restore(r);
        } else {
            panic!("unknown action {action}");
        }
    }
    run.finish();
    let summary = json!({
        "restores": run.restores,
        "final": dump(&run.model),
        "discarded": run.discarded,
        "fed_through": run.fed_through,
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary).expect("summary"),
    )
    .expect("write summary");
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
