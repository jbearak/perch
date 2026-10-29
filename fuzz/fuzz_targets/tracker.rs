// SPDX-License-Identifier: GPL-3.0-only

//! The tracker against `perch-vt`'s test observer. A disagreement panics;
//! an abort inside `perch-vt` ends the process. Both are findings.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = perch::differential::check_fuzz_input(data) {
        panic!("{e}");
    }
});
