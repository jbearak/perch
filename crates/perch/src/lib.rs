// SPDX-License-Identifier: GPL-3.0-only

//! Perch: persistent terminal sessions.

pub mod cli;
pub mod vt_ffi;

/// Perch's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Ghostty commit `perch-vt` is built from, read from
/// `perch-vt/build.zig.zon` by the build script.
pub const GHOSTTY_COMMIT: &str = env!("PERCH_GHOSTTY_COMMIT");
