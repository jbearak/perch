// SPDX-License-Identifier: GPL-3.0-only

//! `perch self-install <dest>`, hidden: the extension's way to keep a
//! stable copy of the binary. Prints one line of JSON.

use std::path::Path;
use std::process::ExitCode;

use serde_json::json;

use super::{fail, usage_error};
use crate::selfinstall;

pub(super) fn run(args: &[String]) -> ExitCode {
    let [dest] = args else {
        return usage_error("`self-install` takes one directory");
    };
    let dest = Path::new(dest);
    if !dest.is_absolute() {
        return usage_error("self-install: the directory must be an absolute path");
    }
    let source = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail(&format!("self-install: cannot find this binary: {e}")),
    };
    match selfinstall::install(&source, &own_version(), dest) {
        Ok(o) => {
            let report = json!({
                "schema": 1,
                "path": o.path,
                "installed": o.installed,
                "locked": o.locked,
                "previous": o.previous,
            });
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("self-install: {e}")),
    }
}

/// The version this binary installs as. Test builds can pretend to be
/// another version with `PERCH_TEST_VERSION`.
fn own_version() -> String {
    #[cfg(feature = "test-hooks")]
    {
        if let Ok(v) = std::env::var("PERCH_TEST_VERSION") {
            return v;
        }
    }
    crate::VERSION.to_owned()
}
