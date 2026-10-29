// SPDX-License-Identifier: GPL-3.0-only

//! Fuzz target `frame_decode`: random bytes, cut into random chunks, through
//! the frame decoder and the parsers of every frame a supervisor reads. The
//! decoder must never panic, must agree with a one-shot decode of the same
//! bytes, and every frame it returns must re-encode to the bytes it came
//! from.

#![no_main]

use libfuzzer_sys::fuzz_target;
use perch::proto::{self, Decoder, Hello, kind};

/// Decodes `data` fed in chunks whose sizes come from `cuts`.
fn decode(data: &[u8], cuts: &[u8]) -> (Vec<proto::Frame>, Option<proto::DecodeError>) {
    let mut d = Decoder::new();
    let mut frames = Vec::new();
    let mut rest = data;
    let mut cuts = cuts.iter().cycle();
    while !rest.is_empty() {
        let n = (usize::from(*cuts.next().unwrap_or(&255)) + 1).min(rest.len());
        d.push(&rest[..n]);
        rest = &rest[n..];
        loop {
            match d.next_frame() {
                Ok(Some(f)) => frames.push(f),
                Ok(None) => break,
                Err(e) => return (frames, Some(e)),
            }
        }
    }
    (frames, None)
}

fuzz_target!(|input: &[u8]| {
    let Some((&ncuts, rest)) = input.split_first() else {
        return;
    };
    let ncuts = usize::from(ncuts % 16).min(rest.len());
    let (cuts, data) = rest.split_at(ncuts);

    let (chunked, err_chunked) = decode(data, if cuts.is_empty() { &[255] } else { cuts });
    let (whole, err_whole) = decode(data, &[255]);
    assert_eq!(chunked, whole);
    assert_eq!(err_chunked, err_whole);

    let mut offset = 0;
    for f in &chunked {
        let bytes = proto::encode(f.kind, &f.payload).expect("a decoded frame re-encodes");
        assert_eq!(&data[offset..offset + bytes.len()], &bytes[..]);
        offset += bytes.len();
        // What the supervisor parses from each frame must not panic.
        match f.kind {
            kind::HELLO => {
                if let Ok(h) = serde_json::from_slice::<Hello>(&f.payload) {
                    let theirs = perch::meta::ProtocolRange {
                        min: h.min,
                        max: h.max,
                    };
                    if let Some(v) = proto::negotiate(proto::PROTOCOL, theirs) {
                        assert!(theirs.min <= v && v <= theirs.max);
                        assert!(proto::PROTOCOL.min <= v && v <= proto::PROTOCOL.max);
                    }
                }
            }
            _ => {
                let _ = serde_json::from_slice::<serde_json::Value>(&f.payload);
            }
        }
    }
});
