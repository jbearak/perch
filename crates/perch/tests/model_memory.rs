// SPDX-License-Identifier: GPL-3.0-only

//! The model's memory at the history and width limits: 10,000 lines of
//! history at 500 columns, every cell printed with a color change every 8
//! cells, must stay under 120 MB of peak resident memory. The numbers for
//! 1,000 and 10,000 lines at 120 and 500 columns are recorded in
//! docs/research/libghostty-build.md.

mod common;

use common::{describe, testprog};
use serde_json::Value;

const LIMIT: u64 = 120 * 1000 * 1000;

#[test]
fn model_stays_under_120_mb_at_10000_lines_by_500_columns() {
    let out = std::process::Command::new(testprog("model-dump"))
        .args(["--memory", "10000", "500"])
        .output()
        .expect("run model-dump");
    assert!(out.status.success(), "{}", describe(&out));
    let v: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let rss = v["max_rss"].as_u64().unwrap();
    assert!(rss < LIMIT, "peak resident memory {rss} bytes: {v}");
    // History is pruned by whole pages, so the line limit is approximate.
    assert!(v["history_rows"].as_u64().unwrap() > 9000, "{v}");
}
