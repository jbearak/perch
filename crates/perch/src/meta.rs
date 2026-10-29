// SPDX-License-Identifier: GPL-3.0-only

//! `meta.json`: a session's metadata, written atomically by writing a
//! temporary file and renaming it.
//!
//! Frozen forever: `format`, `id`, `state`, `exit`, `supervisorPid`,
//! `supervisorStart`, `pid`, `pidStart` and `protocol`. Everything else is
//! additive. Readers ignore fields they do not know, and a rewrite keeps
//! them, so an older `perch` marking a newer supervisor's session `lost`
//! does not drop what the newer one recorded.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::fd::BorrowedFd;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::sys;

/// The metadata format this version writes.
pub const FORMAT: u32 = 1;

/// The file name inside a session directory.
pub const FILE: &str = "meta.json";

/// Published, program not started yet.
pub const STARTING: &str = "starting";
/// The program started.
pub const RUNNING: &str = "running";
/// The program ended while detached; a tombstone.
pub const EXITED: &str = "exited";
/// The supervisor died; a tombstone.
pub const LOST: &str = "lost";

/// The protocol versions a supervisor speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

/// How the program ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    /// The exit code, if it exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// The signal's name, such as `SIGKILL`, if a signal ended it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// The signal's number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_number: Option<i32>,
    /// Why the program could not start, if it never did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Exit {
    /// From a wait status.
    pub fn from_status(status: sys::WaitStatus) -> Exit {
        match status {
            sys::WaitStatus::Exited(code) => Exit {
                code: Some(code),
                ..Exit::default()
            },
            sys::WaitStatus::Signaled(sig) => Exit {
                signal: Some(
                    sys::signal_name(sig).map_or_else(|| format!("SIG{sig}"), str::to_owned),
                ),
                signal_number: Some(sig),
                ..Exit::default()
            },
        }
    }

    /// `exit code 0`, `killed by signal 9 (SIGKILL)`, or the error.
    pub fn describe(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        if let Some(c) = self.code {
            return format!("exit code {c}");
        }
        match (self.signal_number, &self.signal) {
            (Some(n), Some(s)) => format!("killed by signal {n} ({s})"),
            (None, Some(s)) => format!("killed by {s}"),
            (Some(n), None) => format!("killed by signal {n}"),
            (None, None) => "unknown".to_owned(),
        }
    }
}

/// The contents of `meta.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub format: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub created_at: String,
    pub supervisor_pid: i32,
    pub supervisor_start: String,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub pid_start: Option<String>,
    #[serde(default)]
    pub perch_version: String,
    pub protocol: ProtocolRange,
    pub state: String,
    #[serde(default)]
    pub exit: Option<Exit>,
    #[serde(default)]
    pub degraded: Vec<String>,
    /// History lines the model keeps (at most 10,000).
    #[serde(default)]
    pub scrollback: u32,
    /// When the session became a tombstone, for the 7-day expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Fields from newer versions, kept on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Meta {
    /// Whether this is an exited or lost session.
    pub fn is_tombstone(&self) -> bool {
        self.state == EXITED || self.state == LOST
    }

    /// Whether [`read`] can read this back.
    pub fn fits(&self) -> bool {
        self.to_json().len() as u64 <= MAX_SIZE
    }

    /// The JSON written to the file.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("metadata serializes");
        s.push('\n');
        s
    }
}

/// The largest `meta.json` Perch reads.
const MAX_SIZE: u64 = 1 << 20;

/// Reads `meta.json` inside a session directory.
pub fn read(dir: BorrowedFd<'_>) -> io::Result<Meta> {
    let file = sys::open_file_at(dir, OsStr::new(FILE), sys::OpenMode::Read)?;
    let mut text = String::new();
    file.take(MAX_SIZE).read_to_string(&mut text)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes `meta.json` inside a session directory: a temporary file, then a
/// rename over the old one.
pub fn write(dir: BorrowedFd<'_>, meta: &Meta) -> io::Result<()> {
    let tmp = format!(".{FILE}.{}.tmp", crate::runtime::random_name(8));
    let tmp = OsStr::new(&tmp);
    let result = (|| {
        let mut file = sys::open_file_at(dir, tmp, sys::OpenMode::CreateNew)?;
        file.write_all(meta.to_json().as_bytes())?;
        file.sync_data()?;
        drop(file);
        sys::rename_at(dir, tmp, dir, OsStr::new(FILE))
    })();
    if result.is_err() {
        let _ = sys::unlink_at(dir, tmp, false);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Meta {
        Meta {
            format: FORMAT,
            id: "k3x9q2ab".into(),
            name: "R: thesis".into(),
            kind: "r".into(),
            owner: Some("jbearak.raven".into()),
            annotations: BTreeMap::from([("raven.console".into(), "ipc".into())]),
            command: vec!["raven".into(), "console".into()],
            cwd: "/home/me/thesis".into(),
            host: "login2".into(),
            created_at: "2026-10-29T15:04:05Z".into(),
            supervisor_pid: 41200,
            supervisor_start: "linux:81234567".into(),
            pid: Some(41201),
            pid_start: Some("linux:81234570".into()),
            perch_version: "0.1.0".into(),
            protocol: ProtocolRange { min: 1, max: 1 },
            state: RUNNING.into(),
            exit: None,
            degraded: vec![],
            scrollback: 1000,
            ended_at: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn round_trips() {
        let m = sample();
        let back: Meta = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn keeps_unknown_fields_on_rewrite() {
        let mut v: Value = serde_json::from_str(&sample().to_json()).unwrap();
        v["futureField"] = serde_json::json!({"a": [1, 2]});
        v["exit"] = serde_json::json!({"code": 3, "coreDumped": true});
        let mut m: Meta = serde_json::from_value(v).unwrap();
        m.state = LOST.into();
        let again: Value = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(again["futureField"], serde_json::json!({"a": [1, 2]}));
        assert_eq!(again["exit"]["coreDumped"], true);
        assert_eq!(again["state"], "lost");
    }

    #[test]
    fn describes_exits() {
        assert_eq!(
            Exit::from_status(sys::WaitStatus::Exited(3)).describe(),
            "exit code 3"
        );
        assert_eq!(
            Exit::from_status(sys::WaitStatus::Signaled(sys::sig::KILL)).describe(),
            "killed by signal 9 (SIGKILL)"
        );
    }
}
