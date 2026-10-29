// SPDX-License-Identifier: GPL-3.0-only

//! The model process: forked from the supervisor, without `exec`, so it
//! works even after an update has deleted the binary. It holds no
//! descriptors but its end of the socket pair and its stderr, which stays
//! on `supervisor.log` so a Zig safety abort leaves its message there. It
//! reads frames, feeds them to `perch-vt`, sends back DSR 6 replies tagged
//! with their frames, OSC 7 changes and snapshots, and exits when the
//! socket closes.

use std::os::fd::{AsRawFd, OwnedFd};

use super::wire::{Decoder, FromModel, ToModel};
use crate::sys;
use crate::vt_ffi::Model;

/// Ghostty's byte limit on history; the line limit is the session's.
pub const SCROLLBACK_BYTES: u64 = 64 << 20;

/// What a model is created with.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub generation: u32,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: u32,
    /// The address-space limit on Linux.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub memory_limit: u64,
}

/// Runs in the child after `fork`, and never returns.
pub fn main(sock: OwnedFd, params: Params) -> ! {
    let fd = sock.as_raw_fd();
    // Nothing of the supervisor's but the socket and stderr.
    sys::close_fds_from(0, &[2, fd]);
    sys::reset_signals();
    let _ = sys::ignore_signal(sys::sig::PIPE);
    #[cfg(target_os = "linux")]
    if let Err(e) = sys::limit_address_space(params.memory_limit) {
        eprintln!("perch model: cannot limit its address space: {e}");
    }
    #[cfg(feature = "test-hooks")]
    test_hooks::list_fds();
    // A panic must not unwind into the supervisor's stack, which this
    // process inherited; the default hook has already logged it.
    let code = match std::panic::catch_unwind(|| run(fd, params)) {
        Ok(()) => 0,
        Err(_) => 101,
    };
    sys::exit_now(code)
}

fn run(fd: i32, params: Params) {
    let Ok(mut model) = Model::new(
        params.cols,
        params.rows,
        params.scrollback,
        SCROLLBACK_BYTES,
    ) else {
        eprintln!("perch model: out of memory creating the terminal");
        return;
    };
    let generation = params.generation;
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut out = Vec::new();
    let mut pwd: Option<Vec<u8>> = None;
    #[cfg_attr(not(feature = "test-hooks"), allow(unused_mut))]
    let mut hooks = Hooks::default();
    loop {
        let n = match sys::read(fd, &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            let msg = match decoder.next_to_model() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("perch model: unreadable input from the supervisor: {e}");
                    return;
                }
            };
            match msg {
                ToModel::Frame { n, bytes } => model.feed_slice(&bytes, n),
                ToModel::Snapshot { id, history_rows } => {
                    // Replies to earlier frames go first.
                    send_replies(&mut model, generation, &mut out, &hooks);
                    let tail = model.tail(history_rows).unwrap_or_default();
                    FromModel::Snapshot {
                        generation,
                        id,
                        at_ground: model.at_ground(),
                        tail,
                    }
                    .encode(&mut out);
                }
                #[cfg(feature = "test-hooks")]
                ToModel::Fault(f) => hooks.stage(f, generation),
            }
        }
        send_replies(&mut model, generation, &mut out, &hooks);
        let now = model.pwd();
        if now != pwd {
            if let Some(url) = &now {
                FromModel::Pwd {
                    generation,
                    url: url.clone(),
                }
                .encode(&mut out);
            }
            pwd = now;
        }
        if !out.is_empty() {
            if sys::write_all(fd, &out).is_err() {
                return;
            }
            out.clear();
        }
    }
}

#[cfg(feature = "test-hooks")]
type Hooks = test_hooks::Faults;
#[cfg(not(feature = "test-hooks"))]
type Hooks = NoHooks;

/// Production builds change nothing about replies.
#[cfg(not(feature = "test-hooks"))]
#[derive(Default)]
struct NoHooks;

#[cfg(not(feature = "test-hooks"))]
impl ReplyHooks for NoHooks {}

/// What can change how replies are sent: test faults, or nothing.
trait ReplyHooks {
    /// Whether to send no replies.
    fn drop_replies(&self) -> bool {
        false
    }
    /// A generation to send a wrong reply from first.
    fn stale_generation(&self) -> Option<u32> {
        None
    }
}

fn send_replies(model: &mut Model, generation: u32, out: &mut Vec<u8>, hooks: &impl ReplyHooks) {
    for r in model.take_replies() {
        if hooks.drop_replies() {
            continue;
        }
        if let Some(stale) = hooks.stale_generation() {
            FromModel::Reply {
                generation: stale,
                frame: r.frame,
                row: 999,
                col: 999,
            }
            .encode(out);
        }
        FromModel::Reply {
            generation,
            frame: r.frame,
            row: r.row,
            col: r.col,
        }
        .encode(out);
    }
}

#[cfg(feature = "test-hooks")]
mod test_hooks {
    use super::ReplyHooks;
    use crate::model::wire::Fault;
    use crate::sys;

    /// Faults staged by the supervisor's `test-model` op.
    #[derive(Debug, Default)]
    pub struct Faults {
        drop_replies: bool,
        stale: Option<u32>,
    }

    impl Faults {
        pub fn stage(&mut self, f: Fault, generation: u32) {
            match f {
                Fault::Hang => loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                },
                Fault::Abort => crate::vt_ffi::test_abort(),
                Fault::Grow => grow(),
                Fault::DropReplies => self.drop_replies = true,
                Fault::StaleReplies => self.stale = Some(generation.wrapping_sub(1)),
            }
        }
    }

    impl ReplyHooks for Faults {
        fn drop_replies(&self) -> bool {
            self.drop_replies
        }
        fn stale_generation(&self) -> Option<u32> {
            self.stale
        }
    }

    /// Allocates until the memory limit ends the process: on Linux an
    /// allocation fails at the address-space limit and aborts; on macOS the
    /// supervisor sees the resident size and kills it, so the pages are
    /// touched, up to a little past the limit, and then held.
    fn grow() -> ! {
        const STEP: usize = 16 << 20;
        let mut held: Vec<Vec<u8>> = Vec::new();
        let limit = crate::model::memory_limit();
        loop {
            let mut v: Vec<u8> = Vec::with_capacity(STEP);
            if cfg!(target_os = "macos") {
                if (held.len() * STEP) as u64 > limit + (64 << 20) {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                    continue;
                }
                v.resize(STEP, 1);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            held.push(std::hint::black_box(v));
        }
    }

    /// With `PERCH_TEST_MODEL_FDS` set, writes the open descriptors and
    /// their kinds to that file, scanned before it is opened.
    pub fn list_fds() {
        let Some(path) = std::env::var_os("PERCH_TEST_MODEL_FDS") else {
            return;
        };
        let mut text = String::new();
        for fd in (0..65_536).filter(|&fd| sys::fd_is_open(fd)) {
            let kind = if sys::fd_is_socket(fd) {
                "socket"
            } else {
                "file"
            };
            text.push_str(&format!("{fd} {kind}\n"));
        }
        let tmp = std::path::PathBuf::from(&path).with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
