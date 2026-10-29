// SPDX-License-Identifier: GPL-3.0-only

//! What Perch reads about other processes: opaque start times, owners and
//! the members of a terminal session (the kill scope).
//!
//! Start times are opaque strings compared only for equality. On Linux they
//! are `linux:<ticks>`, field 22 of `/proc/<pid>/stat`; on macOS
//! `macos:<sec>.<usec>` from `proc_pidinfo`. Both are exact, so reading one
//! again always gives the same string.

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;

    /// The fields of `/proc/<pid>/stat` after the process name, which is
    /// in parentheses and can itself contain `)` and spaces, so the split
    /// is at the last `)`. Index 0 is field 3 (state).
    pub(crate) fn stat_fields(stat: &str) -> Option<Vec<&str>> {
        let close = stat.rfind(')')?;
        Some(stat[close + 1..].split_ascii_whitespace().collect())
    }

    fn stat(pid: i32) -> Option<String> {
        fs::read_to_string(format!("/proc/{pid}/stat")).ok()
    }

    pub fn start_time_from_stat(stat: &str) -> Option<String> {
        // Field 22 (starttime) is index 22 - 3.
        let f = stat_fields(stat)?;
        let ticks: u64 = f.get(19)?.parse().ok()?;
        Some(format!("linux:{ticks}"))
    }

    pub fn start_time(pid: i32) -> Option<String> {
        start_time_from_stat(&stat(pid)?)
    }

    pub fn real_uid(pid: i32) -> Option<u32> {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = status.lines().find(|l| l.starts_with("Uid:"))?;
        line.split_ascii_whitespace().nth(1)?.parse().ok()
    }

    pub fn is_zombie(pid: i32) -> bool {
        stat(pid)
            .as_deref()
            .and_then(stat_fields)
            .is_some_and(|f| f.first() == Some(&"Z"))
    }

    pub fn session_members(sid: i32) -> Vec<i32> {
        let Ok(dir) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter(|&pid| {
                let Some(text) = stat(pid) else { return false };
                let Some(f) = stat_fields(&text) else {
                    return false;
                };
                // Field 3 (state) is index 0, field 6 (session) index 3.
                f.first() != Some(&"Z") && f.get(3).and_then(|s| s.parse::<i32>().ok()) == Some(sid)
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use crate::sys;

    pub fn start_time(pid: i32) -> Option<String> {
        let info = sys::proc_bsd_info(pid)?;
        Some(format!("macos:{}.{:06}", info.start_sec, info.start_usec))
    }

    pub fn real_uid(pid: i32) -> Option<u32> {
        Some(sys::proc_bsd_info(pid)?.ruid)
    }

    pub fn is_zombie(pid: i32) -> bool {
        sys::proc_bsd_info(pid).is_some_and(|i| i.zombie)
    }

    pub fn session_members(sid: i32) -> Vec<i32> {
        sys::list_all_pids()
            .into_iter()
            .filter(|&pid| sys::getsid(pid).ok() == Some(sid) && !is_zombie(pid))
            .collect()
    }
}

/// Whether `pid` has exited and waits to be reaped. A zombie is out of the
/// kill scope: it cannot be signaled and holds nothing.
pub use imp::is_zombie;
pub use imp::{real_uid, session_members, start_time};

#[cfg(target_os = "linux")]
pub use imp::start_time_from_stat;

/// Whether `pid` is still the process recorded with `start`, and belongs to
/// `uid`. A different start time means the PID was reused.
pub fn is_same_process(pid: i32, start: &str, uid: u32) -> bool {
    pid > 0 && start_time(pid).as_deref() == Some(start) && real_uid(pid) == Some(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_start_time_is_stable() {
        let me = crate::sys::getpid();
        let a = start_time(me).expect("start time");
        let b = start_time(me).expect("start time");
        assert_eq!(a, b);
        assert!(is_same_process(me, &a, crate::sys::getuid()));
        assert!(!is_same_process(me, "linux:1", crate::sys::getuid()));
    }

    #[test]
    fn own_session_includes_self() {
        let sid = crate::sys::getsid(0).unwrap();
        let members = session_members(sid);
        assert!(members.contains(&crate::sys::getpid()), "{members:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_parsing_survives_parentheses_and_spaces() {
        // A process named "a) b (c" with fields after it; starttime is 777.
        let line =
            "4242 (a) b (c) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 777 1000 10";
        assert_eq!(start_time_from_stat(line).as_deref(), Some("linux:777"));
        let line = "17 ()) )) R 1 17 99 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 31337 0 0";
        assert_eq!(start_time_from_stat(line).as_deref(), Some("linux:31337"));
        assert_eq!(start_time_from_stat("17 (x"), None);
    }
}
