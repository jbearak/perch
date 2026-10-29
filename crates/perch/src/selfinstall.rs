// SPDX-License-Identifier: GPL-3.0-only

//! `perch self-install <dest>`: keeps a stable copy of this binary at
//! `<dest>/perch` for terminals and plain SSH, since the extension's own
//! path changes with every update.
//!
//! Several VS Code windows running different versions of the extension may
//! install at once. Under an exclusive `flock` on `<dest>/.lock`, the copy
//! is replaced only by a newer version, read from `<dest>/perch.version`.
//! Where `flock` fails, as on some NFS home directories, every caller
//! installs its own version, so an older one can win until the newer one
//! runs again. Either way the binary and the version file are each written
//! to a temporary file and renamed into place, so neither is ever torn.

use std::cmp::Ordering;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::runtime::random_name;
use crate::sys::{self, Lock};

/// The installed binary's name inside `<dest>`.
pub const BINARY: &str = "perch";
/// The installed binary's version, one line.
pub const VERSION_FILE: &str = "perch.version";
/// The lock file.
pub const LOCK_FILE: &str = ".lock";
/// Prefix of temporary files.
const TEMP_PREFIX: &str = ".tmp-";
/// How long to wait for another installer to finish.
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// Temporary files older than this were left by a crashed installer.
const STALE_TEMP: Duration = Duration::from_secs(3600);

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `<dest>/perch`.
    pub path: PathBuf,
    /// Whether this call replaced the copy.
    pub installed: bool,
    /// Whether the lock was held. Without it, every call installs.
    pub locked: bool,
    /// The version the copy had before, if known.
    pub previous: Option<String>,
}

/// Copies `source` (version `version`) to `<dest>/perch`, creating `dest`.
pub fn install(source: &Path, version: &str, dest: &Path) -> Result<Outcome, String> {
    fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let lock_path = dest.join(LOCK_FILE);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .open(&lock_path)
        .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
    let locked = match lock_exclusive(&lock_file) {
        Ok(true) => true,
        Ok(false) => {
            return Err(format!(
                "another install into {} did not finish within {} seconds",
                dest.display(),
                LOCK_WAIT.as_secs()
            ));
        }
        // flock not supported here: install without it.
        Err(_) => false,
    };
    let path = dest.join(BINARY);
    let previous = installed_version(dest);
    let needed = !locked
        || !path.is_file()
        || previous
            .as_deref()
            .is_none_or(|p| compare_versions(version, p) == Ordering::Greater);
    if locked {
        remove_stale_temps(dest);
    }
    if needed {
        let bytes =
            fs::read(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
        write_replace(dest, BINARY, &bytes, 0o755)?;
        write_replace(dest, VERSION_FILE, format!("{version}\n").as_bytes(), 0o644)?;
    }
    // Dropping the file releases the lock.
    drop(lock_file);
    Ok(Outcome {
        path,
        installed: needed,
        locked,
        previous,
    })
}

/// The version recorded next to the copy.
pub fn installed_version(dest: &Path) -> Option<String> {
    let text = fs::read_to_string(dest.join(VERSION_FILE)).ok()?;
    let v = text.trim();
    parse_version(v).map(|_| v.to_owned())
}

/// Waits for an exclusive `flock`. `Ok(false)` if another holder kept it
/// past [`LOCK_WAIT`]; `Err` if `flock` itself fails.
fn lock_exclusive(file: &File) -> io::Result<bool> {
    #[cfg(feature = "test-hooks")]
    {
        if std::env::var_os("PERCH_TEST_NO_FLOCK").is_some() {
            return Err(io::Error::other("flock disabled by PERCH_TEST_NO_FLOCK"));
        }
    }
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if sys::try_flock(file.as_fd(), Lock::Exclusive)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Writes `bytes` to a temporary file in `dir` and renames it over `name`.
fn write_replace(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
    let temp = dir.join(format!("{TEMP_PREFIX}{}", random_name(12)));
    let result = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        f.write_all(bytes)?;
        // The umask may have cleared bits the copy needs.
        sys::fchmod(f.as_fd(), mode)?;
        f.sync_all()?;
        fs::rename(&temp, dir.join(name))
    })();
    result.map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("cannot install {}: {e}", dir.join(name).display())
    })
}

/// Removes temporary files a crashed installer left behind. Called only
/// under the lock.
fn remove_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let stale = e.file_name().to_string_lossy().starts_with(TEMP_PREFIX)
            && e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > STALE_TEMP);
        if stale {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// `major.minor.patch`, with an optional pre-release after `-` and build
/// metadata after `+`.
fn parse_version(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let v = v.split_once('+').map_or(v, |(core, _)| core);
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        Some(_) => return None,
        None => (v, None),
    };
    let mut parts = core.split('.');
    let mut nums = [0u64; 3];
    for n in &mut nums {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *n = p.parse().ok()?;
    }
    parts.next().is_none().then_some((nums, pre))
}

/// Orders two versions as semantic versioning does. A version that does not
/// parse sorts below every one that does.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some((na, pa)), Some((nb, pb))) => na.cmp(&nb).then_with(|| match (pa, pb) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => compare_prerelease(x, y),
        }),
    }
}

fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let mut xs = a.split('.');
    let mut ys = b.split('.');
    loop {
        match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_as_semver() {
        let ordered = [
            "0.0.9",
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.beta",
            "0.1.0-beta.2",
            "0.1.0-beta.11",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "0.10.0",
            "1.0.0",
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(compare_versions(a, b), i.cmp(&j), "{a} vs {b}");
            }
        }
        assert_eq!(compare_versions("0.1.0+abc", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn unparsable_versions_sort_lowest() {
        for bad in [
            "", "1", "1.2", "1.2.3.4", "1.2.x", "v1.2.3", "1.2.3-", "1..3",
        ] {
            assert!(parse_version(bad).is_none(), "{bad}");
            assert_eq!(compare_versions(bad, "0.0.0"), Ordering::Less, "{bad}");
        }
    }
}
