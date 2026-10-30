// SPDX-License-Identifier: GPL-3.0-only

//! `perch send <session> [--text TEXT | --stdin] [--paste] [--submit]`:
//! queues input for the session's program, as if typed.
//!
//! `queued` means the bytes will be written in order unless the session
//! ends first. If the connection drops before that answer, the outcome is
//! unknown: the send may or may not have been queued, and it is never
//! repeated.

use std::io::Read;
use std::process::ExitCode;

use serde_json::{Value, json};

use super::session::{REQUEST_TIMEOUT, connect, find};
use super::{Args, fail, usage_error};
use crate::base64;
use crate::client::ClientError;

/// The session's input queue had no room; nothing was queued.
pub const EXIT_QUEUE_FULL: u8 = 3;
/// The connection dropped before the session answered; the input may or
/// may not have been sent.
pub const EXIT_OUTCOME_UNKNOWN: u8 = 4;

pub(super) fn run(args: &[String]) -> ExitCode {
    let mut session = None;
    let mut text = None;
    let mut stdin = false;
    let mut paste = false;
    let mut submit = false;
    let mut a = Args::new(args);
    let parsed: Result<(), String> = (|| {
        while let Some(flag) = a.next_arg() {
            match flag {
                "--text" => text = Some(a.value(flag)?.to_owned()),
                "--stdin" => stdin = true,
                "--paste" => paste = true,
                "--submit" => submit = true,
                f if f.starts_with('-') => return Err(format!("send: unknown option `{f}`")),
                s if session.is_none() => session = Some(s.to_owned()),
                s => return Err(format!("send: unexpected argument `{s}`")),
            }
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return usage_error(&e);
    }
    let Some(session) = session else {
        return usage_error("send: which session?");
    };
    let data = match (text, stdin) {
        (Some(_), true) => return usage_error("send: --text or --stdin, not both"),
        (Some(t), false) => t.into_bytes(),
        (None, true) => {
            let mut buf = Vec::new();
            if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
                return fail(&format!("cannot read stdin: {e}"));
            }
            buf
        }
        (None, false) if submit => Vec::new(),
        (None, false) => return usage_error("send: give --text TEXT, --stdin or --submit"),
    };
    let (rt, e) = match find(&session) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    if !e.live {
        return fail(&format!(
            "session {} has ended ({}); nothing was sent",
            e.meta.id, e.meta.state
        ));
    }
    let mut req = json!({ "paste": paste, "submit": submit });
    match String::from_utf8(data) {
        Ok(s) => req["data"] = Value::String(s),
        Err(e) => req["dataBase64"] = Value::String(base64::encode(e.as_bytes())),
    }
    // Test hook: the supervisor queues the send and drops the connection
    // before answering.
    #[cfg(feature = "test-hooks")]
    if std::env::var_os("PERCH_TEST_SEND_DROP").is_some() {
        req["testDrop"] = Value::Bool(true);
    }
    let mut c = match connect(&rt, &e, REQUEST_TIMEOUT) {
        Ok(c) => c,
        Err(err) => {
            return fail(&format!(
                "cannot reach session {}: {err}; nothing was sent",
                e.meta.id
            ));
        }
    };
    if !c.welcome.capabilities.iter().any(|op| op == "send") {
        return fail(&format!(
            "session {} was started by a perch that cannot send; nothing was sent",
            e.meta.id
        ));
    }
    match c.request("send", req, REQUEST_TIMEOUT) {
        Ok(_) => ExitCode::SUCCESS,
        Err(ClientError::Failed { code, .. }) if code == "queue_full" => {
            eprintln!(
                "perch: the input queue of session {} is full; nothing was sent",
                e.meta.id
            );
            ExitCode::from(EXIT_QUEUE_FULL)
        }
        Err(err @ (ClientError::Failed { .. } | ClientError::Refused(_))) => fail(&format!(
            "session {} refused the send: {err}; nothing was sent",
            e.meta.id
        )),
        Err(ClientError::TooLarge) => fail("the input is over the 1 MiB limit; nothing was sent"),
        // The request may have reached the supervisor: never resend.
        Err(err) => {
            eprintln!(
                "perch: the connection to session {} ended before it answered ({err}); the input may not have been sent",
                e.meta.id
            );
            ExitCode::from(EXIT_OUTCOME_UNKNOWN)
        }
    }
}
