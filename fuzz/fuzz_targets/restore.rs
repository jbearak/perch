// SPDX-License-Identifier: GPL-3.0-only

//! A random stream into a model through the gate, a restore built from it
//! and fed to a second `perch-vt` at the client's size, which must agree
//! with the model, then more output to both. A disagreement panics; an
//! abort inside `perch-vt` ends the process. Both are findings. The seed
//! corpus is the fidelity fixtures' streams (see fuzz.yml).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = perch::restore_check::check_fuzz_input(data) {
        panic!("{e}");
    }
});
