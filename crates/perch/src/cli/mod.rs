// SPDX-License-Identifier: GPL-3.0-only

//! The command line.

mod version;

use std::process::ExitCode;

const USAGE: &str = "usage: perch version [--json]";

/// Run the command in `args` (without the program name).
pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("version") => version::run(&args[1..]),
        Some("--version" | "-V") if args.len() == 1 => version::run(&[]),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unknown command `{other}`")),
        None => usage_error("missing command"),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("perch: {msg}\n{USAGE}");
    ExitCode::from(2)
}
