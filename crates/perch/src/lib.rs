// SPDX-License-Identifier: GPL-3.0-only

//! Perch: persistent terminal sessions.

pub mod cli;
pub mod client;
pub mod clock;
pub mod flow;
pub mod input;
pub mod meta;
pub mod procinfo;
pub mod proto;
pub mod queries;
pub mod runtime;
pub mod supervisor;
pub mod sys;
pub mod transport;
pub mod vt_ffi;

#[cfg(test)]
mod scenarios;

/// Perch's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Ghostty commit `perch-vt` is built from, read from
/// `perch-vt/build.zig.zon` by the build script.
pub const GHOSTTY_COMMIT: &str = env!("PERCH_GHOSTTY_COMMIT");
