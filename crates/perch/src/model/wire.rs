// SPDX-License-Identifier: GPL-3.0-only

//! Messages between the supervisor and the model process, over their
//! socket pair. Each is a 4-byte big-endian length (of the type byte and
//! the payload), a type byte and a payload of fixed-width big-endian
//! fields, then any bytes. Both ends are the same binary, so there is no
//! versioning.
//!
//! Everything the model sends carries its generation, so the supervisor
//! can discard what an older model process said.

use std::fmt;

/// The largest message either side accepts. Output frames are far smaller;
/// a snapshot of 1,000 history lines at 500 columns with styles is the
/// largest thing sent.
pub const MAX_MESSAGE: usize = 64 << 20;

mod kind {
    pub const FRAME: u8 = 1;
    pub const SNAPSHOT: u8 = 2;
    #[cfg(feature = "test-hooks")]
    pub const FAULT: u8 = 9;
    pub const REPLY: u8 = 16;
    pub const PWD: u8 = 17;
    pub const SNAPSHOT_DONE: u8 = 18;
}

/// A fault the model process stages on request, in test-hooks builds.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Stop reading the socket, and stay alive.
    Hang = 1,
    /// A Zig safety abort inside `perch-vt`.
    Abort = 2,
    /// Allocate until the memory limit ends the process.
    Grow = 3,
    /// Keep working, but send no DSR 6 replies.
    DropReplies = 4,
    /// Before each DSR 6 reply, send a wrong one tagged with the previous
    /// generation.
    StaleReplies = 5,
}

#[cfg(feature = "test-hooks")]
impl Fault {
    pub fn parse(name: &str) -> Option<Fault> {
        Some(match name {
            "hang" => Fault::Hang,
            "abort" => Fault::Abort,
            "grow" => Fault::Grow,
            "drop-replies" => Fault::DropReplies,
            "stale-replies" => Fault::StaleReplies,
            _ => return None,
        })
    }

    fn from_byte(b: u8) -> Option<Fault> {
        Some(match b {
            1 => Fault::Hang,
            2 => Fault::Abort,
            3 => Fault::Grow,
            4 => Fault::DropReplies,
            5 => Fault::StaleReplies,
            _ => return None,
        })
    }
}

/// From the supervisor to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToModel {
    /// Output bytes, numbered. A frame that ends with a query names it.
    Frame { n: u64, bytes: Vec<u8> },
    /// Send the tail (`perch_vt_tail`) with this many history rows.
    Snapshot { id: u64, history_rows: u32 },
    #[cfg(feature = "test-hooks")]
    Fault(Fault),
}

/// From the model to the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromModel {
    /// The DSR 6 reply to the query that ends frame `frame`.
    Reply {
        generation: u32,
        frame: u64,
        row: u32,
        col: u32,
    },
    /// The working directory the program last reported with OSC 7.
    Pwd { generation: u32, url: Vec<u8> },
    /// The answer to [`ToModel::Snapshot`], taken after every frame sent
    /// before the request.
    Snapshot {
        generation: u32,
        id: u64,
        at_ground: bool,
        tail: Vec<u8>,
    },
}

impl FromModel {
    pub fn generation(&self) -> u32 {
        match self {
            FromModel::Reply { generation, .. }
            | FromModel::Pwd { generation, .. }
            | FromModel::Snapshot { generation, .. } => *generation,
        }
    }
}

/// Appends a message's header to `out`: the length of the type byte and a
/// payload of `len` bytes, then the type.
fn header(out: &mut Vec<u8>, kind: u8, len: usize) {
    let total = u32::try_from(len + 1).expect("message under 4 GiB");
    out.extend_from_slice(&total.to_be_bytes());
    out.push(kind);
}

impl ToModel {
    /// Appends the encoded message to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ToModel::Frame { n, bytes } => encode_frame(out, *n, bytes),
            ToModel::Snapshot { id, history_rows } => {
                header(out, kind::SNAPSHOT, 12);
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&history_rows.to_be_bytes());
            }
            #[cfg(feature = "test-hooks")]
            ToModel::Fault(f) => {
                header(out, kind::FAULT, 1);
                out.push(*f as u8);
            }
        }
    }

    fn decode(kind: u8, p: &[u8]) -> Result<ToModel, WireError> {
        let mut r = Reader(p);
        let msg = match kind {
            kind::FRAME => ToModel::Frame {
                n: r.u64()?,
                bytes: r.rest(),
            },
            kind::SNAPSHOT => ToModel::Snapshot {
                id: r.u64()?,
                history_rows: r.u32()?,
            },
            #[cfg(feature = "test-hooks")]
            kind::FAULT => ToModel::Fault(Fault::from_byte(r.u8()?).ok_or(WireError::Malformed)?),
            other => return Err(WireError::UnknownType(other)),
        };
        Ok(msg)
    }
}

/// Appends a frame message without building a [`ToModel`] first.
pub fn encode_frame(out: &mut Vec<u8>, n: u64, bytes: &[u8]) {
    header(out, kind::FRAME, 8 + bytes.len());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(bytes);
}

impl FromModel {
    /// Appends the encoded message to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            FromModel::Reply {
                generation,
                frame,
                row,
                col,
            } => {
                header(out, kind::REPLY, 20);
                out.extend_from_slice(&generation.to_be_bytes());
                out.extend_from_slice(&frame.to_be_bytes());
                out.extend_from_slice(&row.to_be_bytes());
                out.extend_from_slice(&col.to_be_bytes());
            }
            FromModel::Pwd { generation, url } => {
                header(out, kind::PWD, 4 + url.len());
                out.extend_from_slice(&generation.to_be_bytes());
                out.extend_from_slice(url);
            }
            FromModel::Snapshot {
                generation,
                id,
                at_ground,
                tail,
            } => {
                header(out, kind::SNAPSHOT_DONE, 13 + tail.len());
                out.extend_from_slice(&generation.to_be_bytes());
                out.extend_from_slice(&id.to_be_bytes());
                out.push(u8::from(*at_ground));
                out.extend_from_slice(tail);
            }
        }
    }

    fn decode(kind: u8, p: &[u8]) -> Result<FromModel, WireError> {
        let mut r = Reader(p);
        Ok(match kind {
            kind::REPLY => FromModel::Reply {
                generation: r.u32()?,
                frame: r.u64()?,
                row: r.u32()?,
                col: r.u32()?,
            },
            kind::PWD => FromModel::Pwd {
                generation: r.u32()?,
                url: r.rest(),
            },
            kind::SNAPSHOT_DONE => FromModel::Snapshot {
                generation: r.u32()?,
                id: r.u64()?,
                at_ground: r.u8()? != 0,
                tail: r.rest(),
            },
            other => return Err(WireError::UnknownType(other)),
        })
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        if self.0.len() < N {
            return Err(WireError::Malformed);
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(head.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_be_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_be_bytes(self.take()?))
    }

    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).to_vec()
    }
}

/// A message that cannot be read: the other side is broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    TooLarge(usize),
    UnknownType(u8),
    Malformed,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::TooLarge(n) => write!(f, "a message of {n} bytes"),
            WireError::UnknownType(t) => write!(f, "a message of unknown type {t}"),
            WireError::Malformed => write!(f, "a malformed message"),
        }
    }
}

/// Collects bytes and yields whole messages.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    off: usize,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // What was read is dropped here, so the buffer holds at most one
        // partial message and the bytes just pushed.
        if self.off > 0 {
            self.buf.drain(..self.off);
            self.off = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    fn next_raw(&mut self) -> Result<Option<(u8, &[u8])>, WireError> {
        let avail = &self.buf[self.off..];
        let Some(len) = avail.get(..4) else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(len.try_into().expect("4 bytes")) as usize;
        if len == 0 {
            return Err(WireError::Malformed);
        }
        if len > MAX_MESSAGE {
            return Err(WireError::TooLarge(len));
        }
        if avail.len() < 4 + len {
            return Ok(None);
        }
        let start = self.off + 4;
        self.off += 4 + len;
        let msg = &self.buf[start..start + len];
        Ok(Some((msg[0], &msg[1..])))
    }

    pub fn next_to_model(&mut self) -> Result<Option<ToModel>, WireError> {
        match self.next_raw()? {
            Some((kind, p)) => ToModel::decode(kind, p).map(Some),
            None => Ok(None),
        }
    }

    pub fn next_from_model(&mut self) -> Result<Option<FromModel>, WireError> {
        match self.next_raw()? {
            Some((kind, p)) => FromModel::decode(kind, p).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_in_any_split() {
        let to = [
            ToModel::Frame {
                n: 7,
                bytes: b"ab\x1b[6n".to_vec(),
            },
            ToModel::Frame {
                n: 8,
                bytes: Vec::new(),
            },
            ToModel::Snapshot {
                id: 3,
                history_rows: 1000,
            },
        ];
        let from = [
            FromModel::Reply {
                generation: 2,
                frame: 7,
                row: 1,
                col: 3,
            },
            FromModel::Pwd {
                generation: 2,
                url: b"file://h/tmp".to_vec(),
            },
            FromModel::Snapshot {
                generation: 2,
                id: 3,
                at_ground: true,
                tail: b"one\n".to_vec(),
            },
        ];
        let mut a = Vec::new();
        to.iter().for_each(|m| m.encode(&mut a));
        let mut b = Vec::new();
        from.iter().for_each(|m| m.encode(&mut b));
        for split in 0..a.len().max(b.len()) {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for part in [&a[..split.min(a.len())], &a[split.min(a.len())..]] {
                d.push(part);
                while let Some(m) = d.next_to_model().unwrap() {
                    got.push(m);
                }
            }
            assert_eq!(got, to);
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for part in [&b[..split.min(b.len())], &b[split.min(b.len())..]] {
                d.push(part);
                while let Some(m) = d.next_from_model().unwrap() {
                    got.push(m);
                }
            }
            assert_eq!(got, from);
        }
    }

    #[test]
    fn the_buffer_does_not_grow() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for n in 0..10_000u64 {
            out.clear();
            encode_frame(&mut out, n, &[b'x'; 100]);
            d.push(&out[..50]);
            d.push(&out[50..]);
            assert_eq!(
                d.next_to_model().unwrap(),
                Some(ToModel::Frame {
                    n,
                    bytes: vec![b'x'; 100]
                })
            );
        }
        assert!(d.buf.len() < 1 << 18);
    }

    #[test]
    fn rejects_what_it_cannot_read() {
        let mut d = Decoder::new();
        d.push(&[0, 0, 0, 0]);
        assert_eq!(d.next_from_model(), Err(WireError::Malformed));
        let mut d = Decoder::new();
        d.push(&u32::MAX.to_be_bytes());
        assert!(matches!(d.next_from_model(), Err(WireError::TooLarge(_))));
        let mut d = Decoder::new();
        d.push(&[0, 0, 0, 2, kind::REPLY, 0]);
        assert_eq!(d.next_from_model(), Err(WireError::Malformed));
        let mut d = Decoder::new();
        d.push(&[0, 0, 0, 1, 99]);
        assert_eq!(d.next_from_model(), Err(WireError::UnknownType(99)));
    }
}
