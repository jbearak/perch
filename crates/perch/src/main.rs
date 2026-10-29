// SPDX-License-Identifier: GPL-3.0-only

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    perch::cli::run(&args)
}
