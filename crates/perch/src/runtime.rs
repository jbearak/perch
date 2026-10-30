// SPDX-License-Identifier: GPL-3.0-only

//! The runtime directory, `/tmp/perch-<uid>/` or `$PERCH_RUNTIME_DIR`, and
//! the session directories in it.
//!
//! Every process checks the runtime directory with `lstat` before using it:
//! a real directory, owned by the current uid, mode 0700. Inside it, paths
//! are opened relative to directory descriptors without following symlinks.
//! A supervisor holds an exclusive `flock` on its session directory for as
//! long as it runs; a free lock means no supervisor. Anything is removed
//! only under an exclusive lock on it, after checking that its path still
//! names the locked inode, by renaming it to `.trash-<random>` first.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clock;
use crate::meta::{self, Meta};
use crate::sys::{self, Lock};

/// A session directory younger than this is never touched by others.
pub const YOUNG: Duration = Duration::from_secs(10);
/// `.new-*` and `.trash-*` directories older than this are removed.
pub const STALE_BUILD: Duration = Duration::from_secs(3600);
/// Tombstones older than this are removed by `list`.
pub const TOMBSTONE_EXPIRY: Duration = Duration::from_secs(7 * 86_400);

/// An open, checked runtime directory.
#[derive(Debug)]
pub struct RuntimeDir {
    pub path: PathBuf,
    pub fd: OwnedFd,
}

/// The runtime directory's path: `$PERCH_RUNTIME_DIR`, made absolute, or
/// `/tmp/perch-<uid>`.
pub fn default_path() -> PathBuf {
    match std::env::var_os("PERCH_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().map_or(p.clone(), |cwd| cwd.join(p))
            }
        }
        None => PathBuf::from(format!("/tmp/perch-{}", sys::getuid())),
    }
}

/// Why a runtime directory is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Symlink,
    NotDirectory,
    Owner(u32),
    Mode(u32),
}

/// Checks what `lstat` says about a runtime directory.
pub fn check_stat(st: &sys::Stat, uid: u32) -> Result<(), Refusal> {
    if st.is_symlink() {
        return Err(Refusal::Symlink);
    }
    if !st.is_dir() {
        return Err(Refusal::NotDirectory);
    }
    if st.uid != uid {
        return Err(Refusal::Owner(st.uid));
    }
    if st.mode & 0o7777 != 0o700 {
        return Err(Refusal::Mode(st.mode & 0o7777));
    }
    Ok(())
}

/// Whether a connection's peer may talk to this supervisor.
pub fn peer_allowed(peer_uid: u32, my_uid: u32) -> bool {
    peer_uid == my_uid
}

/// Opens the runtime directory, creating it (mode 0700) if it does not
/// exist, and refuses one that fails the checks.
pub fn open(path: &Path) -> Result<RuntimeDir, String> {
    let uid = sys::getuid();
    let st = match sys::lstat(path) {
        Ok(st) => st,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match sys::mkdir(path, 0o700) {
                Ok(()) => {
                    // The umask may have taken bits from 0700.
                    if let Ok(fd) = sys::open_dir(path) {
                        let _ = sys::fchmod(fd.as_fd(), 0o700);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
            }
            sys::lstat(path).map_err(|e| format!("{}: {e}", path.display()))?
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let refuse = |why: String| {
        format!(
            "refusing to use {}: {why}. Set PERCH_RUNTIME_DIR to a directory of your own with mode 0700.",
            path.display()
        )
    };
    check_stat(&st, uid).map_err(|r| {
        refuse(match r {
            Refusal::Symlink => "it is a symbolic link".into(),
            Refusal::NotDirectory => "it is not a directory".into(),
            Refusal::Owner(o) => format!("it is owned by uid {o}, not {uid}"),
            Refusal::Mode(m) => format!("its mode is {m:04o}, not 0700"),
        })
    })?;
    let fd = sys::open_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The checked path and the opened directory must be the same.
    let opened = sys::fstat(fd.as_fd()).map_err(|e| e.to_string())?;
    if !opened.same_file(&st) {
        return Err(refuse("it changed while being opened".into()));
    }
    check_stat(&opened, uid).map_err(|_| refuse("it changed while being opened".into()))?;
    Ok(RuntimeDir {
        path: path.to_owned(),
        fd,
    })
}

/// Opens the default runtime directory.
pub fn open_default() -> Result<RuntimeDir, String> {
    open(&default_path())
}

/// `n` random characters from `[a-z0-9]`.
pub fn random_name(n: usize) -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; n * 2];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if ok.is_err() {
        // Fall back on the clock and PID; uniqueness is still checked by
        // O_EXCL and RENAME_NOREPLACE where it matters.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            ^ (u128::from(sys::getpid().unsigned_abs()) << 64);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (seed >> ((i * 7) % 120)) as u8 ^ (i as u8).wrapping_mul(31);
        }
    }
    // Rejection sampling keeps the distribution uniform: 252 = 7 * 36.
    let mut out = String::with_capacity(n);
    for &b in &bytes {
        if out.len() == n {
            break;
        }
        if b < 252 {
            out.push(char::from(ALPHABET[usize::from(b) % 36]));
        }
    }
    while out.len() < n {
        out.push('0');
    }
    out
}

/// A new session ID: 8 random characters from `[a-z0-9]`.
pub fn new_id() -> String {
    random_name(8)
}

/// Whether `s` is a valid session ID.
pub fn valid_id(s: &str) -> bool {
    s.len() == 8
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Seconds since a stat's modification time.
fn age_secs(st: &sys::Stat) -> i64 {
    clock::unix_now() - st.mtime.0
}

/// Removes a directory tree inside `dir`, without following symlinks.
fn remove_tree(dir: BorrowedFd<'_>, name: &OsStr, depth: u32) -> io::Result<()> {
    let st = sys::fstatat(dir, name)?;
    if !st.is_dir() {
        return sys::unlink_at(dir, name, false);
    }
    if depth < 32 {
        let sub = sys::open_dir_at(dir, name)?;
        for child in sys::read_dir_names(sub.as_fd())? {
            match remove_tree(sub.as_fd(), &child, depth + 1) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    sys::unlink_at(dir, name, true)
}

/// Removes `name` from the runtime directory. The caller holds an exclusive
/// lock on `locked`, an open descriptor of it. Checks that the name still
/// names that inode, renames it to `.trash-<random>`, then deletes it.
pub fn remove_locked(rt: BorrowedFd<'_>, name: &OsStr, locked: BorrowedFd<'_>) -> io::Result<()> {
    let held = sys::fstat(locked)?;
    let now = sys::fstatat(rt, name)?;
    if !held.same_file(&now) {
        return Err(io::Error::other("the directory was replaced"));
    }
    let trash = OsString::from(format!(".trash-{}", random_name(8)));
    sys::rename_noreplace_at(rt, name, &trash)?;
    remove_tree(rt, &trash, 0)
}

/// One session found in the runtime directory.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The directory's name, which is the session ID.
    pub dir_name: OsString,
    pub meta: Meta,
    /// The supervisor holds the lock.
    pub live: bool,
}

impl Entry {
    /// The session directory's path.
    pub fn path(&self, rt: &RuntimeDir) -> PathBuf {
        rt.path.join(&self.dir_name)
    }
}

/// What [`scan`] may do besides reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Housekeeping {
    /// Only mark dead supervisors' sessions `lost`.
    MarkLost,
    /// Also remove expired tombstones, dead starting sessions and stale
    /// build and trash directories. `list` does this.
    Full,
}

/// Reads every session in the runtime directory. Never touches a directory
/// younger than 10 seconds or one without `meta.json`.
pub fn scan(rt: &RuntimeDir, hk: Housekeeping) -> Vec<Entry> {
    let Ok(names) = sys::read_dir_names(rt.fd.as_fd()) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = names
        .into_iter()
        .filter_map(|name| {
            if name.as_bytes().starts_with(b".") {
                if hk == Housekeeping::Full {
                    sweep_hidden(rt, &name);
                }
                return None;
            }
            inspect(rt, &name, hk)
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.meta.created_at.as_str(), &a.meta.id).cmp(&(b.meta.created_at.as_str(), &b.meta.id))
    });
    entries
}

/// Removes a `.new-*` or `.trash-*` directory older than an hour.
fn sweep_hidden(rt: &RuntimeDir, name: &OsStr) {
    let b = name.as_bytes();
    if !(b.starts_with(b".new-") || b.starts_with(b".trash-")) {
        return;
    }
    let Ok(st) = sys::fstatat(rt.fd.as_fd(), name) else {
        return;
    };
    if !st.is_dir() || age_secs(&st) < clock::scaled(STALE_BUILD).as_secs() as i64 {
        return;
    }
    let Ok(fd) = sys::open_dir_at(rt.fd.as_fd(), name) else {
        return;
    };
    if sys::try_flock(fd.as_fd(), Lock::Exclusive).unwrap_or(false) {
        let _ = remove_locked(rt.fd.as_fd(), name, fd.as_fd());
    }
}

/// Reads one session directory, following "How `list` reads a session".
fn inspect(rt: &RuntimeDir, name: &OsStr, hk: Housekeeping) -> Option<Entry> {
    let fd = sys::open_dir_at(rt.fd.as_fd(), name).ok()?;
    let entry = |meta, live| Entry {
        dir_name: name.to_owned(),
        meta,
        live,
    };
    if !sys::try_flock(fd.as_fd(), Lock::Shared).ok()? {
        // The supervisor is alive. Read after trying the lock, so a
        // supervisor that records `exited` and releases is not misread.
        return meta::read(fd.as_fd()).ok().map(|m| entry(m, true));
    }
    let mut m = meta::read(fd.as_fd()).ok()?;
    let st = sys::fstat(fd.as_fd()).ok()?;
    let young = age_secs(&st) < clock::scaled(YOUNG).as_secs() as i64;
    match m.state.as_str() {
        meta::EXITED | meta::LOST => {
            if hk == Housekeeping::Full && tombstone_expired(&m, &st) && upgrade(&fd) {
                let _ = remove_locked(rt.fd.as_fd(), name, fd.as_fd());
                return None;
            }
            Some(entry(m, false))
        }
        meta::STARTING => {
            if hk == Housekeeping::Full && !young && upgrade(&fd) {
                let _ = remove_locked(rt.fd.as_fd(), name, fd.as_fd());
            }
            None
        }
        _ => {
            // `running` (or a state from a newer version) with a free lock:
            // the supervisor died. Mark it lost under an exclusive lock,
            // unless the directory is too young to touch: then it is only
            // shown as lost.
            if !young
                && upgrade(&fd)
                && same_inode(rt, name, &fd)
                && let Ok(again) = meta::read(fd.as_fd())
            {
                m = again;
                if !m.is_tombstone() {
                    m.state = meta::LOST.into();
                    m.ended_at = Some(clock::rfc3339(clock::unix_now()));
                    let _ = meta::write(fd.as_fd(), &m);
                }
            } else {
                m.state = meta::LOST.into();
            }
            Some(entry(m, false))
        }
    }
}

/// Converts a held shared lock into an exclusive one, without waiting.
fn upgrade(fd: &OwnedFd) -> bool {
    sys::try_flock(fd.as_fd(), Lock::Exclusive).unwrap_or(false)
}

fn same_inode(rt: &RuntimeDir, name: &OsStr, fd: &OwnedFd) -> bool {
    match (sys::fstat(fd.as_fd()), sys::fstatat(rt.fd.as_fd(), name)) {
        (Ok(a), Ok(b)) => a.same_file(&b),
        _ => false,
    }
}

fn tombstone_expired(m: &Meta, st: &sys::Stat) -> bool {
    let ended = m
        .ended_at
        .as_deref()
        .and_then(clock::parse_rfc3339)
        .unwrap_or(st.mtime.0);
    clock::unix_now() - ended > clock::scaled(TOMBSTONE_EXPIRY).as_secs() as i64
}

/// Finds a session by ID, unique ID prefix or exact name.
pub fn resolve(entries: Vec<Entry>, query: &str) -> Result<Entry, String> {
    if query.is_empty() {
        return Err("empty session name".into());
    }
    let mut entries = entries;
    if let Some(i) = entries.iter().position(|e| e.meta.id == query) {
        return Ok(entries.swap_remove(i));
    }
    let pick = |entries: &mut Vec<Entry>, matches: Vec<usize>, what: &str| match matches.as_slice()
    {
        [] => None,
        [i] => Some(Ok(entries.swap_remove(*i))),
        many => {
            let ids: Vec<&str> = many.iter().map(|&i| entries[i].meta.id.as_str()).collect();
            Some(Err(format!(
                "`{query}` matches more than one session {what}: {}",
                ids.join(", ")
            )))
        }
    };
    let by_name: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].meta.name == query)
        .collect();
    if let Some(r) = pick(&mut entries, by_name, "by name") {
        return r;
    }
    let by_prefix: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].meta.id.starts_with(query))
        .collect();
    if let Some(r) = pick(&mut entries, by_prefix, "by ID prefix") {
        return r;
    }
    Err(format!("no session `{query}`"))
}

/// Expands `{dir}` and `{id}` in `s`; `{{` and `}}` are literal braces. Any
/// other brace is an error, so a typo cannot pass through silently.
pub fn expand_placeholders(s: &str, dir: &str, id: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("{{") {
            out.push('{');
            rest = t;
        } else if let Some(t) = tail.strip_prefix("}}") {
            out.push('}');
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{dir}") {
            out.push_str(dir);
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{id}") {
            out.push_str(id);
            rest = t;
        } else {
            return Err(format!(
                "`{s}`: unknown placeholder or lone brace; use {{dir}}, {{id}}, or {{{{ and }}}} for literal braces"
            ));
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(mode: u32, uid: u32) -> sys::Stat {
        sys::Stat {
            dev: 1,
            ino: 2,
            mode,
            uid,
            mtime: (0, 0),
            atime: (0, 0),
        }
    }

    const DIR: u32 = 0o040_000;
    const LNK: u32 = 0o120_000;

    #[test]
    fn runtime_dir_checks() {
        assert_eq!(check_stat(&stat(DIR | 0o700, 501), 501), Ok(()));
        assert_eq!(
            check_stat(&stat(DIR | 0o700, 0), 501),
            Err(Refusal::Owner(0))
        );
        assert_eq!(
            check_stat(&stat(DIR | 0o755, 501), 501),
            Err(Refusal::Mode(0o755))
        );
        assert_eq!(
            check_stat(&stat(DIR | 0o1700, 501), 501),
            Err(Refusal::Mode(0o1700))
        );
        assert_eq!(
            check_stat(&stat(LNK | 0o777, 501), 501),
            Err(Refusal::Symlink)
        );
        assert_eq!(
            check_stat(&stat(0o100_600, 501), 501),
            Err(Refusal::NotDirectory)
        );
    }

    #[test]
    fn peer_uid_check() {
        assert!(peer_allowed(501, 501));
        assert!(!peer_allowed(0, 501));
        assert!(!peer_allowed(502, 501));
        // A real socket pair reports our own uid.
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(sys::peer_uid(a.as_fd()).unwrap(), sys::getuid());
    }

    #[test]
    fn ids() {
        for _ in 0..100 {
            let id = new_id();
            assert!(valid_id(&id), "{id}");
        }
        assert!(!valid_id("ABCDEFGH"));
        assert!(!valid_id("abc"));
        assert!(!valid_id("abcdefg/"));
    }

    #[test]
    fn placeholders() {
        let e = |s| expand_placeholders(s, "/tmp/perch-1/abc", "abc");
        assert_eq!(e("--ipc {dir}/ipc").unwrap(), "--ipc /tmp/perch-1/abc/ipc");
        assert_eq!(e("{id}-{id}").unwrap(), "abc-abc");
        assert_eq!(e("{{dir}} }}{{").unwrap(), "{dir} }{");
        assert_eq!(e("{{{dir}}}").unwrap(), "{/tmp/perch-1/abc}");
        assert_eq!(e("plain").unwrap(), "plain");
        assert!(e("{other}").is_err());
        assert!(e("a { b").is_err());
        assert!(e("a } b").is_err());
        assert!(e("{dir").is_err());
    }
}
