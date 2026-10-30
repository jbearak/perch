// SPDX-License-Identifier: GPL-3.0-only

//! Linked environment variables: for a variable whose value is a socket
//! path (`SSH_AUTH_SOCK`, `VSCODE_IPC_HOOK_CLI`), the program gets a stable
//! path, `$PERCH_SESSION_DIR/env/<NAME>`, a symlink the supervisor
//! re-points at the attached client's value on every attach. `connect()`
//! follows symlinks, so `ssh-add` or `code` inside the session reach the
//! agent or window of whoever is attached now.
//!
//! A variable that is not set when the session starts stays unset: Perch
//! cannot add a variable to a running process. A client that lacks a linked
//! variable gets a link to a path that does not exist, so programs fail as
//! they would have with a stale value. Detaching leaves the links alone.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use crate::{runtime, sys};

/// The directory inside a session directory that holds the links.
pub const DIR: &str = "env";

/// Variables linked when `perch new` gets no `--link-env`. The design lists
/// `SSH_AUTH_SOCK` and `VSCODE_IPC_HOOK_CLI`, each a default only once a
/// link is shown to work for it. `SSH_AUTH_SOCK` passed both the plan's
/// remote check (agent forwarding over ssh, tests/linux) and its local one
/// (the extension's tests). `VSCODE_IPC_HOOK_CLI` stays opt-in until its
/// Remote-SSH check (docs/verification.md) has been run.
pub const DEFAULT: &[&str] = &["SSH_AUTH_SOCK"];

/// Whether `name` can be a linked variable: a portable environment
/// variable name, which is also a safe file name.
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// Where a link points for a client that lacks the variable: a name in the
/// links directory that nothing creates.
pub fn missing_target(name: &str) -> OsString {
    OsString::from(format!(".unset-{name}"))
}

/// Opens the links directory inside the session directory `dir`, creating
/// it (0700) if it is not there.
fn open_dir(dir: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    match sys::mkdir_at(dir, OsStr::new(DIR), 0o700) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    sys::open_dir_at(dir, OsStr::new(DIR))
}

/// Points every link in `links` (name and target) atomically: a new
/// symlink, then a rename over the old one.
pub fn point(dir: BorrowedFd<'_>, links: &BTreeMap<String, OsString>) -> io::Result<()> {
    if links.is_empty() {
        return Ok(());
    }
    let env = open_dir(dir)?;
    for (name, target) in links {
        let tmp = OsString::from(format!(".new-{name}-{}", runtime::random_name(8)));
        sys::symlink_at(target, env.as_fd(), &tmp)?;
        if let Err(e) = sys::rename_at(env.as_fd(), &tmp, env.as_fd(), OsStr::new(name)) {
            let _ = sys::unlink_at(env.as_fd(), &tmp, false);
            return Err(e);
        }
    }
    Ok(())
}

/// The targets for an attaching client: its value of each linked variable,
/// or [`missing_target`] where it has none. Values for variables the
/// session does not link are ignored.
pub fn targets(names: &[String], client: &BTreeMap<String, String>) -> BTreeMap<String, OsString> {
    names
        .iter()
        .map(|n| {
            let target = match client.get(n) {
                Some(v) if !v.is_empty() && !v.contains('\0') => OsString::from(v),
                _ => missing_target(n),
            };
            (n.clone(), target)
        })
        .collect()
}

/// This process's values of `names`, for an `Attach` frame. Values that
/// are not UTF-8 are left out, as if unset.
pub fn client_values(names: &[String]) -> BTreeMap<String, String> {
    names
        .iter()
        .filter_map(|n| Some((n.clone(), std::env::var(n).ok()?)))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["SSH_AUTH_SOCK", "_X", "a1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "1A", "A-B", "A=B", "../x", ".x", "A B"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn targets_follow_the_client() {
        let names = vec!["A".to_owned(), "B".to_owned()];
        let client = BTreeMap::from([
            ("A".to_owned(), "/tmp/agent.1".to_owned()),
            ("C".to_owned(), "/elsewhere".to_owned()),
        ]);
        let t = targets(&names, &client);
        assert_eq!(t.len(), 2);
        assert_eq!(t["A"], OsString::from("/tmp/agent.1"));
        assert_eq!(t["B"], missing_target("B"));
    }

    #[test]
    fn points_links_atomically() {
        let base = std::env::temp_dir().join(format!("perch-links-{}", runtime::random_name(8)));
        sys::mkdir(&base, 0o700).unwrap();
        let dir = sys::open_dir(&base).unwrap();
        let mut links = BTreeMap::from([("A".to_owned(), OsString::from("/one"))]);
        point(dir.as_fd(), &links).unwrap();
        let env = sys::open_dir_at(dir.as_fd(), OsStr::new(DIR)).unwrap();
        assert_eq!(
            sys::read_link_at(env.as_fd(), OsStr::new("A")).unwrap(),
            "/one"
        );
        links.insert("A".into(), OsString::from("/two"));
        point(dir.as_fd(), &links).unwrap();
        assert_eq!(
            sys::read_link_at(env.as_fd(), OsStr::new("A")).unwrap(),
            "/two"
        );
        // Nothing else is left in the directory.
        assert_eq!(sys::read_dir_names(env.as_fd()).unwrap(), vec!["A"]);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
