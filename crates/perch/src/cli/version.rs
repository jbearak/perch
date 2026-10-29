// SPDX-License-Identifier: GPL-3.0-only

//! `perch version [--json]`: the version, the Ghostty commit and the result
//! of `perch-vt`'s selftest. Exits 1 if the selftest fails.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::vt_ffi;

pub(super) fn run(args: &[String]) -> ExitCode {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return super::usage_error("`version` takes only --json"),
    };
    let selftest = vt_ffi::selftest();
    let ok = selftest.as_ref().is_ok_and(vt_ffi::Selftest::ok);
    if json {
        let report = json!({
            "version": crate::VERSION,
            "ghostty_commit": crate::GHOSTTY_COMMIT,
            "selftest": selftest_json(&selftest, ok),
        });
        println!("{report:#}");
    } else {
        let short = &crate::GHOSTTY_COMMIT[..crate::GHOSTTY_COMMIT.len().min(7)];
        let state = if ok { "ok" } else { "FAILED" };
        println!(
            "perch {} (Ghostty {short}, selftest {state})",
            crate::VERSION
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn selftest_json(selftest: &Result<vt_ffi::Selftest, i32>, ok: bool) -> Value {
    match selftest {
        Ok(s) => json!({
            "ok": ok,
            "cursor": { "col": s.cursor.0, "row": s.cursor.1 },
            "size": { "cols": s.size.0, "rows": s.size.1 },
            "kitty_graphics": s.kitty_graphics,
            "dsr6_reply": String::from_utf8_lossy(&s.dsr6_reply),
            "formatted": String::from_utf8_lossy(&s.formatted),
        }),
        Err(status) => json!({ "ok": false, "status": status }),
    }
}
