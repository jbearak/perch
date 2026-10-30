// SPDX-License-Identifier: GPL-3.0-only

//! The client side of control mode: connect, `Hello`, requests.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::proto::{self, Decoder, ErrorBody, Frame, Hello, Welcome, kind};

/// How long a client gives each `info` call.
pub const INFO_TIMEOUT: Duration = Duration::from_millis(500);

/// Why a control exchange failed.
#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    /// No answer before the deadline.
    Timeout,
    /// The supervisor closed the connection.
    Closed,
    /// An `Error` frame.
    Refused(ErrorBody),
    /// The supervisor answered something this client cannot read.
    Protocol(String),
    /// A response with `ok: false`.
    Failed {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Timeout => write!(f, "the supervisor did not answer in time"),
            ClientError::Closed => write!(f, "the supervisor closed the connection"),
            ClientError::Refused(e) => write!(f, "refused ({}): {}", e.code, e.message),
            ClientError::Protocol(m) => write!(f, "protocol error: {m}"),
            ClientError::Failed { code, message } => write!(f, "{message} ({code})"),
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> ClientError {
        if matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            ClientError::Timeout
        } else {
            ClientError::Io(e)
        }
    }
}

/// A connection in control mode.
pub struct Client {
    stream: UnixStream,
    decoder: Decoder,
    next_id: u64,
    pub welcome: Welcome,
}

impl Client {
    /// Connects to a supervisor's socket and completes the handshake
    /// within `timeout`.
    pub fn connect(sock: &Path, label: &str, timeout: Duration) -> Result<Client, ClientError> {
        let deadline = Instant::now() + timeout;
        let mut stream = UnixStream::connect(sock)?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(&proto::encode_json(kind::HELLO, &Hello::control(label)))?;
        let mut decoder = Decoder::new();
        let frame = read_frame(&mut stream, &mut decoder, deadline)?;
        match frame.kind {
            kind::WELCOME => {
                let welcome: Welcome = serde_json::from_slice(&frame.payload)
                    .map_err(|e| ClientError::Protocol(format!("bad Welcome: {e}")))?;
                Ok(Client {
                    stream,
                    decoder,
                    next_id: 1,
                    welcome,
                })
            }
            kind::ERROR => Err(ClientError::Refused(
                serde_json::from_slice(&frame.payload)
                    .unwrap_or_else(|_| ErrorBody::new("unknown", "unreadable Error frame")),
            )),
            k => Err(ClientError::Protocol(format!(
                "expected Welcome, got frame type {k}"
            ))),
        }
    }

    /// Sends `{"id":..,"op":op,...fields}` and waits up to `timeout` for the
    /// response's `result`.
    pub fn request(
        &mut self,
        op: &str,
        fields: Value,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        let deadline = Instant::now() + timeout;
        let id = self.next_id;
        self.next_id += 1;
        let mut req = json!({ "id": id, "op": op });
        if let (Some(obj), Value::Object(extra)) = (req.as_object_mut(), fields) {
            obj.extend(extra);
        }
        self.stream
            .write_all(&proto::encode_json(kind::REQUEST, &req))?;
        loop {
            let frame = read_frame(&mut self.stream, &mut self.decoder, deadline)?;
            if frame.kind == kind::ERROR {
                return Err(ClientError::Refused(
                    serde_json::from_slice(&frame.payload)
                        .unwrap_or_else(|_| ErrorBody::new("unknown", "unreadable Error frame")),
                ));
            }
            if frame.kind != kind::RESPONSE {
                continue;
            }
            let resp: Value = serde_json::from_slice(&frame.payload)
                .map_err(|e| ClientError::Protocol(format!("bad response: {e}")))?;
            if resp["id"] != json!(id) {
                continue;
            }
            if resp["ok"] == json!(true) {
                return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
            }
            let err = &resp["error"];
            return Err(ClientError::Failed {
                code: err["code"].as_str().unwrap_or("unknown").to_owned(),
                message: err["message"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_owned(),
            });
        }
    }

    /// Waits until the supervisor closes the connection, up to `timeout`.
    pub fn wait_closed(&mut self, timeout: Duration) -> Result<(), ClientError> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ClientError::Timeout);
            }
            self.stream.set_read_timeout(Some(left))?;
            match self.stream.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn read_frame(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    deadline: Instant,
) -> Result<Frame, ClientError> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        if let Some(f) = decoder
            .next_frame()
            .map_err(|e| ClientError::Protocol(e.to_string()))?
        {
            return Ok(f);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ClientError::Timeout);
        }
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut buf) {
            Ok(0) => return Err(ClientError::Closed),
            Ok(n) => decoder.push(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
}
