// SPDX-License-Identifier: GPL-3.0-only

//! Test programs for Perch's integration tests, built against `perch` with
//! the same target and `test-hooks` setting as the test run. Each program
//! is its own binary under `src/bin/`. They make their system calls
//! through `perch::sys`.

use std::io::Write;
use std::path::Path;

/// Writes `contents` to `path` atomically, so a test polling for the file
/// never reads half of it.
pub fn write_atomically(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).expect("create output");
    f.write_all(contents.as_bytes()).expect("write output");
    drop(f);
    std::fs::rename(&tmp, path).expect("rename output");
}
