// SPDX-License-Identifier: GPL-3.0-only

//! The client side: the handshake both modes start with ([`Connection`])
//! and control mode (requests) here, and `perch attach`'s relay in
//! [`attach`].

pub mod attach;

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
    /// The request is over the frame cap; nothing was sent.
    TooLarge,
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
            ClientError::TooLarge => write!(f, "the request is over the 1 MiB frame limit"),
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

impl ClientError {
    /// An `Error` frame's refusal.
    pub(crate) fn refused(frame: &Frame) -> ClientError {
        ClientError::Refused(
            serde_json::from_slice(&frame.payload)
                .unwrap_or_else(|_| ErrorBody::new("unknown", "unreadable Error frame")),
        )
    }
}

/// A connection past `Hello` and `Welcome`, in either mode. Its decoder
/// holds any bytes read after the `Welcome`.
pub(crate) struct Connection {
    pub(crate) stream: UnixStream,
    pub(crate) decoder: Decoder,
    pub(crate) welcome: Welcome,
}

impl Connection {
    /// Sends `hello` on a connected `stream` and reads the `Welcome`, or
    /// the `Error` refusing it, by `deadline`. Writes time out after
    /// `write_timeout`.
    pub(crate) fn handshake(
        mut stream: UnixStream,
        hello: &Hello,
        deadline: Instant,
        write_timeout: Duration,
    ) -> Result<Connection, ClientError> {
        stream.set_write_timeout(Some(write_timeout))?;
        stream.write_all(&proto::encode_json(kind::HELLO, hello))?;
        let mut decoder = Decoder::new();
        let frame = read_frame(&mut stream, &mut decoder, deadline)?;
        match frame.kind {
            kind::WELCOME => {
                let welcome: Welcome = serde_json::from_slice(&frame.payload)
                    .map_err(|e| ClientError::Protocol(format!("bad Welcome: {e}")))?;
                Ok(Connection {
                    stream,
                    decoder,
                    welcome,
                })
            }
            kind::ERROR => Err(ClientError::refused(&frame)),
            k => Err(ClientError::Protocol(format!(
                "expected Welcome, got frame type {k}"
            ))),
        }
    }

    /// The next frame, by `deadline`.
    pub(crate) fn read_frame(&mut self, deadline: Instant) -> Result<Frame, ClientError> {
        read_frame(&mut self.stream, &mut self.decoder, deadline)
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
        let stream = UnixStream::connect(sock)?;
        let Connection {
            stream,
            decoder,
            welcome,
        } = Connection::handshake(stream, &Hello::control(label), deadline, timeout)?;
        Ok(Client {
            stream,
            decoder,
            next_id: 1,
            welcome,
        })
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
        let frame = proto::encode(kind::REQUEST, &serde_json::to_vec(&req).expect("JSON"))
            .map_err(|_| ClientError::TooLarge)?;
        self.stream.write_all(&frame)?;
        loop {
            let frame = read_frame(&mut self.stream, &mut self.decoder, deadline)?;
            if frame.kind == kind::ERROR {
                return Err(ClientError::refused(&frame));
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
            // macOS refuses (EINVAL) once the peer has closed; the read
            // then says so.
            let _ = self.stream.set_read_timeout(Some(left));
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

/// The next frame from `stream`, by `deadline`.
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
        // As in `wait_closed`.
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut buf) {
            Ok(0) => return Err(ClientError::Closed),
            Ok(n) => decoder.push(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
}
