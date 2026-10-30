// SPDX-License-Identifier: GPL-3.0-only

//! `reedline-prompt OUT`: a reedline line editor at the version Raven's
//! console uses, with bracketed paste on. Like Raven's console, it asks
//! the terminal for the cursor position (DSR 6) as it draws each prompt,
//! and crossterm waits up to 2 seconds for the answer.
//!
//! For each line it reads, it appends a JSON line to OUT:
//! `{"t": ms since start, "len": bytes, "head": the first 40 characters}`.
//! The line `ris` makes it write RIS first; `exit` ends it.

use std::io::Write;
use std::time::Instant;

use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use serde_json::json;

fn main() {
    let out = std::env::args().nth(1).expect("usage: reedline-prompt OUT");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .expect("open OUT");
    let start = Instant::now();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("rl".into()),
        DefaultPromptSegment::Empty,
    );
    let mut editor = Reedline::create().use_bracketed_paste(true);
    loop {
        let line = match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => line,
            Ok(_) => continue,
            Err(e) => {
                let rec =
                    json!({ "t": start.elapsed().as_millis() as u64, "error": e.to_string() });
                writeln!(file, "{rec}").expect("record");
                continue;
            }
        };
        let rec = json!({
            "t": start.elapsed().as_millis() as u64,
            "len": line.len(),
            "head": line.chars().take(40).collect::<String>(),
        });
        writeln!(file, "{rec}").expect("record");
        file.flush().expect("flush");
        match line.as_str() {
            "ris" => {
                let mut o = std::io::stdout().lock();
                o.write_all(b"\x1bc").expect("write");
                o.flush().expect("flush");
            }
            "exit" => return,
            _ => {}
        }
    }
}
