// SPDX-License-Identifier: GPL-3.0-only

//! The wire protocol against a real supervisor: a golden transcript of the
//! handshake and control ops, and every protocol error.

#![cfg(feature = "test-hooks")]

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::{Rt, golden};
use perch::proto::{self, Decoder, kind};

/// A raw connection that records every frame both ways.
struct Raw {
    stream: UnixStream,
    decoder: Decoder,
    log: Vec<String>,
}

impl Raw {
    fn connect(rt: &Rt, id: &str) -> Raw {
        let stream = UnixStream::connect(rt.dir(id).join("sock")).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Raw {
            stream,
            decoder: Decoder::new(),
            log: Vec::new(),
        }
    }

    fn send_bytes(&mut self, bytes: &[u8], note: &str) {
        self.stream.write_all(bytes).unwrap();
        self.log.push(format!("C> {note}"));
    }

    fn send(&mut self, k: u8, payload: &str) {
        let bytes = proto::encode(k, payload.as_bytes()).unwrap();
        self.stream.write_all(&bytes).unwrap();
        self.log.push(format!(
            "C> {} {} {payload}",
            hex_header(&bytes),
            kind::name(k)
        ));
    }

    /// The next frame, or `None` once the supervisor closes.
    fn recv(&mut self) -> Option<proto::Frame> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = self
                .decoder
                .next_frame()
                .expect("valid frames from the supervisor")
            {
                let bytes = proto::encode(f.kind, &f.payload).unwrap();
                let text = String::from_utf8_lossy(&f.payload).replace(perch::VERSION, "<version>");
                self.log.push(format!(
                    "S> {} {} {text}",
                    hex_header(&bytes),
                    kind::name(f.kind)
                ));
                return Some(f);
            }
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.log.push("S> (closed)".into());
                    return None;
                }
                Ok(n) => self.decoder.push(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                    self.log.push("S> (closed)".into());
                    return None;
                }
                Err(e) => panic!("read: {e}"),
            }
        }
    }

    fn json_of(&mut self, request: &str) -> serde_json::Value {
        self.send(kind::REQUEST, request);
        self.json()
    }

    fn json(&mut self) -> serde_json::Value {
        let f = self.recv().expect("a frame");
        serde_json::from_slice(&f.payload).unwrap()
    }
}

fn hex_header(frame: &[u8]) -> String {
    frame[..5].iter().map(|b| format!("{b:02x}")).collect()
}

const HELLO: &str = r#"{"min":1,"max":1,"client":"golden-test","mode":"control","label":"test"}"#;

#[test]
fn golden_hello_welcome_error_transcript() {
    let rt = Rt::new();
    let id = rt.new_session(&["--name", "proto", "--", "sleep", "1000"]);
    let mut transcript = Vec::new();

    // A control session.
    let mut c = Raw::connect(&rt, &id);
    c.send(kind::HELLO, HELLO);
    assert_eq!(c.recv().unwrap().kind, kind::WELCOME);
    c.send(kind::REQUEST, r#"{"id":1,"op":"rename","name":"renamed"}"#);
    c.recv();
    c.send(
        kind::REQUEST,
        r#"{"id":"two","op":"annotate","set":{"k":"v"},"unset":["none"],"future":true}"#,
    );
    c.recv();
    c.send(kind::REQUEST, r#"{"id":3,"op":"frobnicate"}"#);
    c.recv();
    c.send(kind::REQUEST, r#"{"id":4,"op":"rename","name":""}"#);
    c.recv();
    c.send(kind::REQUEST, r#"{"id":5}"#);
    c.recv();
    c.send(kind::REQUEST, r#"not json"#);
    c.recv();
    c.send(kind::REQUEST, r#"{"id":6,"op":"detach"}"#);
    c.recv();
    c.send(kind::HELLO, HELLO);
    c.recv();
    c.recv();
    transcript.push("# control session".to_owned());
    transcript.append(&mut c.log);

    // No common version.
    let mut c = Raw::connect(&rt, &id);
    c.send(
        kind::HELLO,
        r#"{"min":2,"max":3,"client":"future","mode":"control","label":""}"#,
    );
    let e = c.json();
    assert_eq!(e["supervisor"], serde_json::json!({"min":1,"max":1}));
    assert_eq!(e["client"], serde_json::json!({"min":2,"max":3}));
    c.recv();
    transcript.push("# version mismatch".to_owned());
    transcript.append(&mut c.log);

    // A request before Hello.
    let mut c = Raw::connect(&rt, &id);
    c.send(kind::REQUEST, r#"{"id":1,"op":"info"}"#);
    c.recv();
    c.recv();
    transcript.push("# request before Hello".to_owned());
    transcript.append(&mut c.log);

    // An unreadable Hello, and an unknown mode.
    for (title, hello) in [
        ("# unreadable Hello", r#"{"min":1}"#),
        (
            "# attach mode (not in this supervisor)",
            r#"{"min":1,"max":1,"mode":"attach"}"#,
        ),
        ("# unknown mode", r#"{"min":1,"max":1,"mode":"spectate"}"#),
    ] {
        let mut c = Raw::connect(&rt, &id);
        c.send(kind::HELLO, hello);
        c.recv();
        c.recv();
        transcript.push(title.to_owned());
        transcript.append(&mut c.log);
    }

    // Broken framing.
    for (title, bytes, note) in [
        (
            "# a zero-length frame",
            vec![0u8, 0, 0, 0],
            "00000000 (length 0)",
        ),
        (
            "# a frame over 1 MiB",
            vec![0x00, 0x10, 0x00, 0x01, 1],
            "00100001 01 (length 1 MiB + 1)",
        ),
    ] {
        let mut c = Raw::connect(&rt, &id);
        c.send_bytes(&bytes, note);
        c.recv();
        c.recv();
        transcript.push(title.to_owned());
        transcript.append(&mut c.log);
    }

    golden("protocol.txt", &(transcript.join("\n") + "\n"));
    assert_eq!(rt.meta(&id)["name"], "renamed");
    assert_eq!(rt.meta(&id)["annotations"], serde_json::json!({ "k": "v" }));
}

#[test]
fn no_hello_within_the_deadline_closes_the_connection() {
    let rt = Rt::new(); // a 0.5-second deadline
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let mut c = Raw::connect(&rt, &id);
    let t = Instant::now();
    // Half a frame does not count.
    c.send_bytes(&[0, 0, 0, 10, 1, b'{'], "a partial Hello");
    assert!(c.recv().is_none());
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(400) && took < Duration::from_secs(4),
        "{took:?}"
    );
    // The supervisor carries on.
    assert_eq!(rt.listed(&id).unwrap()["state"], "running");
}

#[test]
fn an_exact_1_mib_frame_is_accepted() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let mut c = Raw::connect(&rt, &id);
    c.send(kind::HELLO, HELLO);
    assert_eq!(c.recv().unwrap().kind, kind::WELCOME);
    // A request padded to the 1 MiB limit (type byte plus payload).
    let head = r#"{"id":9,"op":"info","pad":""#;
    let tail = r#""}"#;
    let pad = "x".repeat(proto::MAX_FRAME - 1 - head.len() - tail.len());
    let payload = format!("{head}{pad}{tail}");
    let bytes = proto::encode(kind::REQUEST, payload.as_bytes()).unwrap();
    assert_eq!(bytes.len(), 4 + proto::MAX_FRAME);
    c.stream.write_all(&bytes).unwrap();
    let r = c.json();
    assert_eq!(r["id"], 9);
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"]["state"], "running");
}

#[test]
fn oversized_replies_and_metadata_are_refused_and_the_session_survives() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    let mut c = Raw::connect(&rt, &id);
    c.send(kind::HELLO, HELLO);
    assert_eq!(c.recv().unwrap().kind, kind::WELCOME);

    // The reply would echo an `id` that fills the request's whole frame.
    let head = r#"{"op":"info","id":""#;
    let tail = r#""}"#;
    let pad = "x".repeat(proto::MAX_FRAME - 1 - head.len() - tail.len());
    c.send(kind::REQUEST, &format!("{head}{pad}{tail}"));
    let r = c.json();
    assert_eq!(r["error"]["code"], "too_large");

    // Two annotations that each fit a frame but together outgrow meta.json.
    let big = "v".repeat(600 << 10);
    for (n, key) in ["a", "b"].iter().enumerate() {
        let req = serde_json::json!({ "id": n, "op": "annotate", "set": { *key: big } });
        c.send(kind::REQUEST, &req.to_string());
        let r = c.json();
        assert_eq!(r["ok"], n == 0, "{r}");
    }
    assert_eq!(
        c.json_of(r#"{"id":5,"op":"info"}"#)["result"]["state"],
        "running"
    );
    assert!(rt.listed(&id).is_some());
}

#[test]
fn a_client_that_disconnects_mid_frame_does_not_disturb_the_supervisor() {
    let rt = Rt::new();
    let id = rt.new_session(&["--", "sleep", "1000"]);
    for _ in 0..5 {
        let mut c = Raw::connect(&rt, &id);
        c.send_bytes(&[0, 0, 1, 0, 4, b'{', b'"'], "partial");
        drop(c);
        let c = Raw::connect(&rt, &id);
        drop(c.stream.shutdown(std::net::Shutdown::Both));
    }
    // A request sent before the client closes its side is still answered.
    let mut c = Raw::connect(&rt, &id);
    c.send(kind::HELLO, HELLO);
    c.send(kind::REQUEST, r#"{"id":1,"op":"info"}"#);
    c.stream.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(c.recv().unwrap().kind, kind::WELCOME);
    assert_eq!(c.json()["ok"], true);
    assert!(c.recv().is_none());
    assert_eq!(rt.listed(&id).unwrap()["state"], "running");
}
