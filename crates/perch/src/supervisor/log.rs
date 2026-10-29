// SPDX-License-Identifier: GPL-3.0-only

//! `supervisor.log`: the supervisor's stderr, capped at 256 KiB. When it
//! grows past the cap, the older half is dropped.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::os::fd::{AsFd, FromRawFd};

use crate::{clock, sys};

/// The log's size limit.
pub const CAP: u64 = 256 * 1024;

/// The log's file name inside the session directory.
pub const FILE: &str = "supervisor.log";

/// Writes one line to stderr, which is the log, then enforces the cap.
pub fn log(msg: &str) {
    let line = format!(
        "{} perch[{}]: {msg}\n",
        clock::rfc3339(clock::unix_now()),
        sys::getpid()
    );
    let _ = sys::write_all(2, line.as_bytes());
    cap();
}

fn cap() {
    // SAFETY: fd 2 stays open for the process's life; ManuallyDrop keeps
    // this File from closing it.
    let mut file = ManuallyDrop::new(unsafe { File::from_raw_fd(2) });
    let Ok(st) = sys::fstat(file.as_fd()) else {
        return;
    };
    if st.mode & 0o170_000 != 0o100_000 {
        return;
    }
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return;
    };
    if len <= CAP {
        return;
    }
    let keep = CAP / 2;
    let mut tail = Vec::new();
    if file.seek(SeekFrom::Start(len - keep)).is_err() || file.read_to_end(&mut tail).is_err() {
        return;
    }
    // Start at a line boundary.
    let start = tail.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
    if sys::ftruncate(file.as_fd(), 0).is_ok() {
        // The log is opened with O_APPEND, so this lands at the start.
        let _ = sys::write_all(2, b"[earlier lines dropped: log over 256 KiB]\n");
        let _ = sys::write_all(2, &tail[start..]);
    }
}
