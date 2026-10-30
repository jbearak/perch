// SPDX-License-Identifier: GPL-3.0-only

//! The wire protocol between clients and a supervisor.
//!
//! A frame is a 4-byte big-endian length, a 1-byte type and a payload. The
//! length covers the type and the payload and is at most 1 MiB. The frame
//! format, `Hello`, `Welcome`, `Error` and the ops `info` and `kill` are
//! frozen forever; everything else is additive.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::meta::ProtocolRange;

/// The largest value of the length field: type plus payload.
pub const MAX_FRAME: usize = 1 << 20;

/// The protocol versions this build speaks.
pub const PROTOCOL: ProtocolRange = ProtocolRange { min: 1, max: 1 };

/// Frame types. Numbers are frozen once used; attach mode's frames take
/// the next ones.
pub mod kind {
    /// Client's first frame: JSON [`super::Hello`].
    pub const HELLO: u8 = 1;
    /// Supervisor's answer: JSON [`super::Welcome`].
    pub const WELCOME: u8 = 2;
    /// A refusal, after which the supervisor closes: JSON [`super::ErrorBody`].
    pub const ERROR: u8 = 3;
    /// A control-mode request: JSON `{"id":..,"op":..,...}`.
    pub const REQUEST: u8 = 4;
    /// A control-mode response: JSON `{"id":..,"ok":true,"result":{..}}` or
    /// `{"id":..,"ok":false,"error":{"code":..,"message":..}}`.
    pub const RESPONSE: u8 = 5;
    /// Attach mode's first frame after `Welcome`: JSON [`super::Attach`].
    pub const ATTACH: u8 = 6;
    /// The supervisor took the client: JSON [`super::Attached`].
    pub const ATTACHED: u8 = 7;
    /// `ifDetached` was set and another client is live; the supervisor
    /// closes: JSON `{}`.
    pub const BUSY: u8 = 8;
    /// Bytes the client's terminal sent, raw.
    pub const INPUT: u8 = 9;
    /// Bytes for the client's terminal, raw: a restore or live output.
    pub const OUTPUT: u8 = 10;
    /// The client's terminal changed size: JSON [`super::Resize`].
    pub const RESIZE: u8 = 11;
    /// The client is no longer attached: JSON [`super::Detached`].
    pub const DETACHED: u8 = 12;
    /// The program ended; the supervisor closes: JSON [`super::Exited`].
    pub const EXITED: u8 = 13;

    /// A name for transcripts and errors.
    pub fn name(k: u8) -> &'static str {
        match k {
            HELLO => "Hello",
            WELCOME => "Welcome",
            ERROR => "Error",
            REQUEST => "Request",
            RESPONSE => "Response",
            ATTACH => "Attach",
            ATTACHED => "Attached",
            BUSY => "Busy",
            INPUT => "Input",
            OUTPUT => "Output",
            RESIZE => "Resize",
            DETACHED => "Detached",
            EXITED => "Exited",
            _ => "Unknown",
        }
    }
}

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// Encodes a frame. Fails if the payload is too large.
pub fn encode(kind: u8, payload: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let len = payload.len() + 1;
    if len > MAX_FRAME {
        return Err(DecodeError::TooLarge(len));
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&u32::try_from(len).expect("checked").to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Encodes a frame with a JSON payload.
pub fn encode_json(kind: u8, value: &impl Serialize) -> Vec<u8> {
    let payload = serde_json::to_vec(value).expect("JSON serializes");
    encode(kind, &payload).expect("control messages are small")
}

/// Why a byte stream is not a valid sequence of frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// A length of zero: no type byte.
    Empty,
    /// A length over [`MAX_FRAME`].
    TooLarge(usize),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Empty => write!(f, "frame with length 0"),
            DecodeError::TooLarge(n) => write!(f, "frame of {n} bytes is over the 1 MiB limit"),
        }
    }
}

/// An incremental frame decoder. Feed it bytes as they arrive and take
/// frames out. It never holds more than one frame plus the last chunk.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    start: usize,
    failed: Option<DecodeError>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Appends bytes read from the peer.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes received but not yet returned as frames.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// The next complete frame, `Ok(None)` if more bytes are needed, or an
    /// error, after which the stream cannot be resynchronized.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, DecodeError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let avail = &self.buf[self.start..];
        if avail.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(avail[..4].try_into().expect("4 bytes")) as usize;
        let err = match len {
            0 => Some(DecodeError::Empty),
            n if n > MAX_FRAME => Some(DecodeError::TooLarge(n)),
            _ => None,
        };
        if let Some(e) = err {
            self.failed = Some(e);
            return Err(e);
        }
        if avail.len() < 4 + len {
            return Ok(None);
        }
        let frame = Frame {
            kind: avail[4],
            payload: avail[5..4 + len].to_vec(),
        };
        self.start += 4 + len;
        if self.start > 64 * 1024 && self.start * 2 > self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        Ok(Some(frame))
    }
}

/// The client's first frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub min: u32,
    pub max: u32,
    #[serde(default)]
    pub client: String,
    pub mode: String,
    #[serde(default)]
    pub label: String,
}

impl Hello {
    /// A control-mode `Hello` from this build.
    pub fn control(label: &str) -> Hello {
        Hello {
            min: PROTOCOL.min,
            max: PROTOCOL.max,
            client: format!("perch {}", crate::VERSION),
            mode: "control".into(),
            label: label.into(),
        }
    }
}

impl Hello {
    /// An attach-mode `Hello` from this build.
    pub fn attach(label: &str) -> Hello {
        Hello {
            mode: "attach".into(),
            ..Hello::control(label)
        }
    }
}

/// The restore a client asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestoreMode {
    /// History, screens and state, for a terminal that starts empty.
    Full,
    /// Screens and state; the terminal keeps its scrollback.
    Screen,
    /// Nothing: the first attach of `perch new`, before the program runs.
    None,
}

/// The client's attach request. Fields are additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attach {
    pub rows: u16,
    pub cols: u16,
    pub restore: RestoreMode,
    /// The most history rows the client's terminal keeps.
    #[serde(default)]
    pub scrollback: u32,
    /// Attach only if no client is live; otherwise `Busy`.
    #[serde(default)]
    pub if_detached: bool,
    /// The client's values of the linked variables.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// The client's process, for `info`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
}

/// The supervisor's answer to an `Attach` it takes: the size in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub rows: u16,
    pub cols: u16,
}

/// The client's terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resize {
    pub rows: u16,
    pub cols: u16,
}

/// Why a client is no longer attached: `takeover`, `detach` (asked for
/// with the `detach` op) or `too_slow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detached {
    pub reason: String,
}

/// How the program ended, as in `meta.json`'s `exit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exited {
    pub exit: crate::meta::Exit,
    /// The status `perch attach` exits with: the program's code, 128 plus
    /// a signal's number, or the code for a program that never started.
    pub status: u8,
}

/// The supervisor's answer to a `Hello` it accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub perch: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// The payload of an `Error` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    /// The supervisor's protocol range, on a version mismatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<ProtocolRange>,
    /// The client's protocol range, on a version mismatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ProtocolRange>,
}

impl ErrorBody {
    pub fn new(code: &str, message: impl Into<String>) -> ErrorBody {
        ErrorBody {
            code: code.into(),
            message: message.into(),
            supervisor: None,
            client: None,
        }
    }
}

/// The highest version both ranges contain.
pub fn negotiate(ours: ProtocolRange, theirs: ProtocolRange) -> Option<u32> {
    let hi = ours.max.min(theirs.max);
    let lo = ours.min.max(theirs.min);
    (theirs.min <= theirs.max && lo <= hi).then_some(hi)
}

/// A successful control response.
pub fn ok_response(id: &Value, result: Value) -> Value {
    json!({ "id": id, "ok": true, "result": result })
}

/// A failed control response.
pub fn error_response(id: &Value, code: &str, message: &str) -> Value {
    json!({ "id": id, "ok": false, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_every_split() {
        let mut bytes = encode(kind::HELLO, b"{\"a\":1}").unwrap();
        bytes.extend(encode(kind::REQUEST, b"").unwrap());
        for cut in 0..=bytes.len() {
            let mut d = Decoder::new();
            let mut frames = Vec::new();
            for part in [&bytes[..cut], &bytes[cut..]] {
                d.push(part);
                while let Some(f) = d.next_frame().unwrap() {
                    frames.push(f);
                }
            }
            assert_eq!(frames.len(), 2, "cut at {cut}");
            assert_eq!(
                frames[0],
                Frame {
                    kind: kind::HELLO,
                    payload: b"{\"a\":1}".to_vec()
                }
            );
            assert_eq!(
                frames[1],
                Frame {
                    kind: kind::REQUEST,
                    payload: vec![]
                }
            );
            assert_eq!(d.pending(), 0);
        }
    }

    #[test]
    fn rejects_empty_and_oversized() {
        let mut d = Decoder::new();
        d.push(&[0, 0, 0, 0]);
        assert_eq!(d.next_frame(), Err(DecodeError::Empty));
        let mut d = Decoder::new();
        d.push(&((MAX_FRAME as u32) + 1).to_be_bytes());
        assert_eq!(d.next_frame(), Err(DecodeError::TooLarge(MAX_FRAME + 1)));
        // The error sticks.
        d.push(&[1, 2, 3]);
        assert!(d.next_frame().is_err());
        // Exactly 1 MiB is allowed.
        assert!(encode(kind::RESPONSE, &vec![0; MAX_FRAME - 1]).is_ok());
        assert!(encode(kind::RESPONSE, &vec![0; MAX_FRAME]).is_err());
    }

    #[test]
    fn negotiation() {
        let r = |min, max| ProtocolRange { min, max };
        assert_eq!(negotiate(r(1, 1), r(1, 1)), Some(1));
        assert_eq!(negotiate(r(1, 3), r(2, 5)), Some(3));
        assert_eq!(negotiate(r(1, 1), r(2, 3)), None);
        assert_eq!(negotiate(r(1, 1), r(1, 0)), None);
    }
}
