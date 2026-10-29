// SPDX-License-Identifier: GPL-3.0-only

//! Forking the program onto the PTY.
//!
//! Everything the child needs is built before `fork`. The child then makes
//! a new session with the PTY slave as its controlling terminal, restores
//! the creator's umask and directory, clears the signal mask, restores
//! default dispositions, closes every descriptor except 0, 1, 2 and the
//! close-on-exec result pipe, and calls `execve`. If `exec` fails it writes
//! the stage and `errno` to the pipe; if it succeeds, the pipe closes with
//! nothing written.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::sys::{self, CVec};

/// What failed in the child before `exec` replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Setup = 1,
    Chdir = 2,
    Exec = 3,
}

/// A failed start, as read from the result pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError {
    pub stage: Stage,
    pub errno: i32,
}

impl StartError {
    /// Decodes the 5 bytes the child writes.
    pub fn decode(bytes: &[u8]) -> Option<StartError> {
        let stage = match *bytes.first()? {
            1 => Stage::Setup,
            2 => Stage::Chdir,
            3 => Stage::Exec,
            _ => return None,
        };
        let errno = i32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?);
        Some(StartError { stage, errno })
    }

    /// A message for the user.
    pub fn describe(&self, command: &str, cwd: &Path) -> String {
        let err = io::Error::from_raw_os_error(self.errno);
        match self.stage {
            Stage::Setup => format!("cannot set up the terminal for `{command}`: {err}"),
            Stage::Chdir => format!("cannot change to directory {}: {err}", cwd.display()),
            Stage::Exec => format!("cannot run `{command}`: {err}"),
        }
    }

    /// The exit code `perch new` uses: 127 for not found, else 126, as
    /// shells do.
    pub fn exit_code(&self) -> u8 {
        if self.stage == Stage::Exec && self.errno == sys::errno::ENOENT {
            127
        } else {
            126
        }
    }
}

/// Everything the child needs, built before `fork`.
pub struct Plan {
    candidates: Vec<CString>,
    argv: CVec,
    envp: CVec,
    cwd: CString,
    umask: u32,
}

impl Plan {
    /// `command` is the argument vector, `env` the full environment as
    /// `NAME=VALUE` entries.
    pub fn new(command: &[String], env: &[OsString], cwd: &Path, umask: u32) -> io::Result<Plan> {
        let program = command
            .first()
            .ok_or_else(|| io::Error::other("empty command"))?;
        let path_var = env
            .iter()
            .find_map(|e| e.as_bytes().strip_prefix(b"PATH="))
            .map_or_else(
                || OsStr::new("/usr/bin:/bin").to_owned(),
                |p| OsStr::from_bytes(p).to_owned(),
            );
        let candidates = search_candidates(program, &path_var)
            .iter()
            .map(|p| sys::path_cstring(p))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Plan {
            candidates,
            argv: CVec::new(command)?,
            envp: CVec::new(env)?,
            cwd: sys::path_cstring(cwd)?,
            umask,
        })
    }
}

/// Where `execvp` would look for `program`.
fn search_candidates(program: &str, path_var: &OsStr) -> Vec<PathBuf> {
    if program.contains('/') {
        return vec![PathBuf::from(program)];
    }
    path_var
        .as_bytes()
        .split(|&b| b == b':')
        .map(|dir| {
            let dir = if dir.is_empty() {
                Path::new(".")
            } else {
                Path::new(OsStr::from_bytes(dir))
            };
            dir.join(program)
        })
        .collect()
}

/// Forks the program. Returns its PID and the read end of the result pipe.
/// `slave` is the PTY slave, opened by the caller.
pub fn spawn(plan: &Plan, slave: &OwnedFd) -> io::Result<(i32, OwnedFd)> {
    let (result_r, result_w) = sys::pipe()?;
    // SAFETY: the supervisor is single-threaded; the child calls only
    // async-signal-safe functions and allocates nothing.
    match unsafe { sys::fork()? } {
        sys::Fork::Child => child(plan, slave.as_raw_fd(), result_w.as_raw_fd()),
        sys::Fork::Parent(pid) => {
            drop(result_w);
            Ok((pid, result_r))
        }
    }
}

fn fail(pipe: i32, stage: Stage, errno: i32) -> ! {
    let mut msg = [0u8; 5];
    msg[0] = stage as u8;
    msg[1..].copy_from_slice(&errno.to_be_bytes());
    let _ = sys::write_all(pipe, &msg);
    sys::exit_now(127)
}

fn os_errno(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

fn child(plan: &Plan, slave: i32, pipe: i32) -> ! {
    // Signals first, so nothing the supervisor installed runs here.
    sys::reset_signals();
    if let Err(e) = sys::setsid() {
        fail(pipe, Stage::Setup, os_errno(&e));
    }
    if let Err(e) = sys::set_controlling_tty(slave) {
        fail(pipe, Stage::Setup, os_errno(&e));
    }
    for fd in 0..3 {
        if let Err(e) = sys::dup2(slave, fd) {
            fail(pipe, Stage::Setup, os_errno(&e));
        }
    }
    sys::close_fds_from(3, &[pipe]);
    sys::set_umask(plan.umask);
    if let Err(e) = sys::chdir_c(&plan.cwd) {
        fail(pipe, Stage::Chdir, os_errno(&e));
    }
    let mut errno = sys::errno::ENOENT;
    let mut saw_access = false;
    for path in &plan.candidates {
        let e = sys::execve(path, &plan.argv, &plan.envp);
        if sys::exec_error_is_access(e) {
            saw_access = true;
        } else if !sys::exec_error_means_not_here(e) {
            errno = e;
            break;
        }
        errno = e;
    }
    if saw_access && sys::exec_error_means_not_here(errno) {
        errno = sys::errno::EACCES;
    }
    fail(pipe, Stage::Exec, errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates() {
        assert_eq!(
            search_candidates("/bin/sh", OsStr::new("/x")),
            vec![PathBuf::from("/bin/sh")]
        );
        assert_eq!(
            search_candidates("./a", OsStr::new("/x")),
            vec![PathBuf::from("./a")]
        );
        assert_eq!(
            search_candidates("sh", OsStr::new("/a::/b")),
            vec![
                PathBuf::from("/a/sh"),
                PathBuf::from("./sh"),
                PathBuf::from("/b/sh")
            ]
        );
    }

    #[test]
    fn start_error_codec() {
        let mut b = vec![3];
        b.extend_from_slice(&2i32.to_be_bytes());
        let e = StartError::decode(&b).unwrap();
        assert_eq!(
            e,
            StartError {
                stage: Stage::Exec,
                errno: 2
            }
        );
        assert_eq!(e.exit_code(), 127);
        assert_eq!(StartError::decode(&[3, 0]), None);
    }
}
