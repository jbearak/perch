// SPDX-License-Identifier: GPL-3.0-only

//! Every `libc` call Perch makes. Everything else goes through these thin,
//! safe wrappers, so the unsafe surface is in one place.
//!
//! The wrappers return `io::Error` built from `errno`. Functions marked as
//! usable after `fork` allocate nothing and are async-signal-safe.

// Type widths differ between Linux and macOS (`mode_t` is u32 on one and
// u16 on the other), so a conversion needed on one is a no-op on the other.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unnecessary_cast,
    clippy::useless_conversion
)]

use std::ffi::{CStr, CString, OsStr, OsString, c_char, c_int};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

fn cvt(ret: c_int) -> io::Result<c_int> {
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn cvt_retry(mut f: impl FnMut() -> c_int) -> io::Result<c_int> {
    loop {
        match cvt(f()) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

fn cstr(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn errno() -> c_int {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `errno` values callers match on.
pub mod errno {
    pub const EWOULDBLOCK: i32 = libc::EWOULDBLOCK;
    pub const EEXIST: i32 = libc::EEXIST;
    pub const ENOTEMPTY: i32 = libc::ENOTEMPTY;
    pub const ENOENT: i32 = libc::ENOENT;
    pub const ESRCH: i32 = libc::ESRCH;
    pub const EPERM: i32 = libc::EPERM;
    pub const EACCES: i32 = libc::EACCES;
}

// ---------------------------------------------------------------------------
// Identity

/// The real user ID.
pub fn getuid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// This process's ID.
pub fn getpid() -> i32 {
    // SAFETY: getpid cannot fail.
    unsafe { libc::getpid() }
}

/// The session ID of `pid` (0 for the caller).
pub fn getsid(pid: i32) -> io::Result<i32> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::getsid(pid) })
}

/// The host name.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if ret != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Sets the umask and returns the previous one. Usable after `fork`.
pub fn set_umask(mask: u32) -> u32 {
    // SAFETY: umask cannot fail.
    u32::from(unsafe { libc::umask(mask as libc::mode_t) })
}

/// The current umask, read without changing it.
pub fn current_umask() -> u32 {
    let old = set_umask(0o077);
    set_umask(old);
    old
}

/// `chdir`.
pub fn chdir(path: &Path) -> io::Result<()> {
    chdir_c(&cstr(path.as_os_str())?)
}

/// `chdir` with a prepared C string. Usable after `fork`.
pub fn chdir_c(path: &CStr) -> io::Result<()> {
    // SAFETY: valid C string.
    cvt(unsafe { libc::chdir(path.as_ptr()) }).map(drop)
}

// ---------------------------------------------------------------------------
// Files and directories, relative to directory descriptors

/// Metadata from `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub mtime: (i64, i64),
    pub atime: (i64, i64),
}

impl Stat {
    fn from_raw(st: &libc::stat) -> Stat {
        Stat {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            mode: u32::from(st.st_mode),
            uid: st.st_uid,
            mtime: (st.st_mtime as i64, st.st_mtime_nsec as i64),
            atime: (st.st_atime as i64, st.st_atime_nsec as i64),
        }
    }

    pub fn is_dir(&self) -> bool {
        self.mode & (libc::S_IFMT as u32) == libc::S_IFDIR as u32
    }

    pub fn is_symlink(&self) -> bool {
        self.mode & (libc::S_IFMT as u32) == libc::S_IFLNK as u32
    }

    pub fn is_socket(&self) -> bool {
        self.mode & (libc::S_IFMT as u32) == libc::S_IFSOCK as u32
    }

    /// Same file: same device and inode.
    pub fn same_file(&self, other: &Stat) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

/// `lstat`.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    let p = cstr(path.as_os_str())?;
    let mut st = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid pointers.
    cvt(unsafe { libc::lstat(p.as_ptr(), st.as_mut_ptr()) })?;
    // SAFETY: initialized by a successful lstat.
    let st = unsafe { st.assume_init() };
    Ok(Stat::from_raw(&st))
}

/// `fstat`.
pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    let mut st = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid pointer, borrowed descriptor.
    cvt(unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) })?;
    // SAFETY: initialized by a successful fstat.
    let st = unsafe { st.assume_init() };
    Ok(Stat::from_raw(&st))
}

/// `fstatat` with `AT_SYMLINK_NOFOLLOW`.
pub fn fstatat(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<Stat> {
    let n = cstr(name)?;
    let mut st = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid pointers.
    cvt(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            n.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    // SAFETY: initialized by a successful fstatat.
    let st = unsafe { st.assume_init() };
    Ok(Stat::from_raw(&st))
}

fn open_raw(
    dir: Option<BorrowedFd<'_>>,
    path: &OsStr,
    flags: c_int,
    mode: u32,
) -> io::Result<OwnedFd> {
    let p = cstr(path)?;
    let flags = flags | libc::O_CLOEXEC;
    let fd = cvt_retry(|| {
        // SAFETY: valid C string; the mode argument is read only with O_CREAT.
        unsafe {
            match dir {
                Some(d) => libc::openat(d.as_raw_fd(), p.as_ptr(), flags, mode as libc::c_uint),
                None => libc::open(p.as_ptr(), flags, mode as libc::c_uint),
            }
        }
    })?;
    // SAFETY: a fresh descriptor we own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens a directory without following a symlink in the last component.
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    open_raw(
        None,
        path.as_os_str(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
}

/// Opens `name` inside `dir` as a directory, without following symlinks.
pub fn open_dir_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<OwnedFd> {
    open_raw(
        Some(dir),
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
}

/// How to open a file with [`open_file_at`].
#[derive(Debug, Clone, Copy)]
pub enum OpenMode {
    Read,
    /// Create a new file; fail if it exists.
    CreateNew,
    /// Create or open for appending.
    Append,
}

/// Opens a regular file inside `dir`, never following a symlink.
pub fn open_file_at(dir: BorrowedFd<'_>, name: &OsStr, how: OpenMode) -> io::Result<std::fs::File> {
    let flags = libc::O_NOFOLLOW
        | match how {
            OpenMode::Read => libc::O_RDONLY,
            OpenMode::CreateNew => libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            OpenMode::Append => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
        };
    open_raw(Some(dir), name, flags, 0o600).map(std::fs::File::from)
}

/// Opens a path read-write without making it a controlling terminal.
pub fn open_rw_noctty(path: &Path) -> io::Result<OwnedFd> {
    open_raw(None, path.as_os_str(), libc::O_RDWR | libc::O_NOCTTY, 0)
}

/// `mkdirat`.
pub fn mkdir_at(dir: BorrowedFd<'_>, name: &OsStr, mode: u32) -> io::Result<()> {
    let n = cstr(name)?;
    // SAFETY: valid C string.
    cvt(unsafe { libc::mkdirat(dir.as_raw_fd(), n.as_ptr(), mode as libc::mode_t) }).map(drop)
}

/// `mkdir` of an absolute path.
pub fn mkdir(path: &Path, mode: u32) -> io::Result<()> {
    let p = cstr(path.as_os_str())?;
    // SAFETY: valid C string.
    cvt(unsafe { libc::mkdir(p.as_ptr(), mode as libc::mode_t) }).map(drop)
}

/// `renameat`, replacing the target if it exists (as `rename` does).
pub fn rename_at(
    dir: BorrowedFd<'_>,
    from: &OsStr,
    to_dir: BorrowedFd<'_>,
    to: &OsStr,
) -> io::Result<()> {
    let f = cstr(from)?;
    let t = cstr(to)?;
    // SAFETY: valid C strings.
    cvt(unsafe { libc::renameat(dir.as_raw_fd(), f.as_ptr(), to_dir.as_raw_fd(), t.as_ptr()) })
        .map(drop)
}

/// `renameat` that fails with `EEXIST` instead of replacing an existing
/// target: `renameat2(RENAME_NOREPLACE)` on Linux, `renameatx_np(RENAME_EXCL)`
/// on macOS. Where the kernel or file system lacks it, falls back to checking
/// first; a non-empty target still makes `rename` fail.
pub fn rename_noreplace_at(dir: BorrowedFd<'_>, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let f = cstr(from)?;
    let t = cstr(to)?;
    let d = dir.as_raw_fd();
    #[cfg(target_os = "linux")]
    // SAFETY: valid C strings; renameat2 takes these arguments.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            d,
            f.as_ptr(),
            d,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as c_int
    };
    #[cfg(target_os = "macos")]
    // SAFETY: valid C strings.
    let ret = unsafe { libc::renameatx_np(d, f.as_ptr(), d, t.as_ptr(), libc::RENAME_EXCL) };
    if ret == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL | libc::ENOTSUP) => {
            if fstatat(dir, to).is_ok() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST));
            }
            rename_at(dir, from, dir, to)
        }
        _ => Err(err),
    }
}

/// `unlinkat`; `dir_entry` removes a directory.
pub fn unlink_at(dir: BorrowedFd<'_>, name: &OsStr, dir_entry: bool) -> io::Result<()> {
    let n = cstr(name)?;
    let flags = if dir_entry { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: valid C string.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), n.as_ptr(), flags) }).map(drop)
}

/// Sets the access and modification times of `name` inside `dir` to now,
/// without following a symlink.
pub fn touch_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    let n = cstr(name)?;
    // SAFETY: a null times pointer means "now".
    cvt(unsafe {
        libc::utimensat(
            dir.as_raw_fd(),
            n.as_ptr(),
            std::ptr::null(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })
    .map(drop)
}

/// Sets both times of `name` inside `dir` to `secs` since the epoch,
/// without following a symlink.
pub fn set_times_at(dir: BorrowedFd<'_>, name: &OsStr, secs: i64) -> io::Result<()> {
    let n = cstr(name)?;
    let t = libc::timespec {
        tv_sec: secs as libc::time_t,
        tv_nsec: 0,
    };
    let times = [t, t];
    // SAFETY: valid pointers.
    cvt(unsafe {
        libc::utimensat(
            dir.as_raw_fd(),
            n.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })
    .map(drop)
}

/// `fchmod`.
pub fn fchmod(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) }).map(drop)
}

/// The names in a directory, without `.` and `..`.
pub fn read_dir_names(dir: BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
    // fdopendir takes ownership of its descriptor, so give it a duplicate,
    // and rewind because the duplicate shares the offset.
    // SAFETY: plain syscall.
    let dup = cvt(unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
    // SAFETY: dup is ours; on failure we close it.
    let dirp = unsafe { libc::fdopendir(dup) };
    if dirp.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: still ours.
        unsafe { libc::close(dup) };
        return Err(err);
    }
    let mut names = Vec::new();
    // SAFETY: dirp is a valid DIR until closedir; entries are read before
    // the next readdir.
    unsafe {
        libc::rewinddir(dirp);
        loop {
            let ent = libc::readdir(dirp);
            if ent.is_null() {
                break;
            }
            let name = CStr::from_ptr((*ent).d_name.as_ptr());
            let bytes = name.to_bytes();
            if bytes != b"." && bytes != b".." {
                names.push(OsString::from_vec(bytes.to_vec()));
            }
        }
        libc::closedir(dirp);
    }
    Ok(names)
}

/// Kinds of `flock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Shared,
    Exclusive,
    Unlock,
}

/// Non-blocking `flock`. Returns `Ok(false)` if another holder has it.
pub fn try_flock(fd: BorrowedFd<'_>, lock: Lock) -> io::Result<bool> {
    let op = match lock {
        Lock::Shared => libc::LOCK_SH,
        Lock::Exclusive => libc::LOCK_EX,
        Lock::Unlock => libc::LOCK_UN,
    } | libc::LOCK_NB;
    // SAFETY: plain syscall.
    match cvt_retry(|| unsafe { libc::flock(fd.as_raw_fd(), op) }) {
        Ok(_) => Ok(true),
        Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(false),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Descriptors

/// A pipe, both ends close-on-exec.
pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: valid array of two.
    cvt(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    // SAFETY: fresh descriptors we own.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    set_cloexec(r.as_raw_fd(), true)?;
    set_cloexec(w.as_raw_fd(), true)?;
    Ok((r, w))
}

/// Sets or clears `FD_CLOEXEC`.
pub fn set_cloexec(fd: RawFd, on: bool) -> io::Result<()> {
    // SAFETY: plain fcntl calls.
    unsafe {
        let flags = cvt(libc::fcntl(fd, libc::F_GETFD))?;
        let new = if on {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        cvt(libc::fcntl(fd, libc::F_SETFD, new)).map(drop)
    }
}

/// Sets or clears `O_NONBLOCK`.
pub fn set_nonblocking(fd: RawFd, on: bool) -> io::Result<()> {
    // SAFETY: plain fcntl calls.
    unsafe {
        let flags = cvt(libc::fcntl(fd, libc::F_GETFL))?;
        let new = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        cvt(libc::fcntl(fd, libc::F_SETFL, new)).map(drop)
    }
}

/// Whether `fd` is open. Usable after `fork`.
pub fn fd_is_open(fd: RawFd) -> bool {
    // SAFETY: F_GETFD on any number is harmless.
    unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
}

/// `dup2`. Usable after `fork`.
pub fn dup2(from: RawFd, to: RawFd) -> io::Result<()> {
    // SAFETY: plain syscall.
    cvt_retry(|| unsafe { libc::dup2(from, to) }).map(drop)
}

/// Closes a raw descriptor. Usable after `fork`.
pub fn close(fd: RawFd) {
    // SAFETY: the caller owns fd.
    unsafe { libc::close(fd) };
}

/// The highest descriptor number worth scanning.
fn fd_scan_limit() -> RawFd {
    // SAFETY: plain call.
    let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if max <= 0 {
        65_536
    } else {
        max.min(65_536) as RawFd
    }
}

/// Closes every descriptor from `lowest` up, except those in `keep`.
/// Usable after `fork`: it allocates nothing.
pub fn close_fds_from(lowest: RawFd, keep: &[RawFd]) {
    #[cfg(target_os = "linux")]
    {
        // close_range over the gaps between kept descriptors. `keep` is
        // tiny, so a fixed array keeps this allocation-free.
        let mut kept = [RawFd::MAX; 8];
        let mut n = 0;
        for &k in keep.iter().filter(|&&k| k >= lowest) {
            if n == kept.len() {
                n = usize::MAX;
                break;
            }
            kept[n] = k;
            n += 1;
        }
        if n != usize::MAX {
            kept[..n].sort_unstable();
            let mut from = lowest;
            let mut ok = true;
            for &k in kept[..n].iter().chain(std::iter::once(&RawFd::MAX)) {
                if k > from {
                    let last = if k == RawFd::MAX {
                        libc::c_uint::MAX
                    } else {
                        (k - 1) as libc::c_uint
                    };
                    // SAFETY: plain syscall; ENOSYS on old kernels.
                    ok &= unsafe {
                        libc::syscall(libc::SYS_close_range, from as libc::c_uint, last, 0)
                    } == 0;
                }
                if k != RawFd::MAX {
                    from = k + 1;
                }
            }
            if ok {
                return;
            }
        }
    }
    let limit = fd_scan_limit();
    for fd in lowest..limit {
        if !keep.contains(&fd) {
            // SAFETY: closing a descriptor number we were told to drop.
            unsafe { libc::close(fd) };
        }
    }
}

/// `read` on a raw descriptor, retrying on `EINTR`.
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: valid buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// `write` on a raw descriptor, retrying on `EINTR`. Usable after `fork`.
pub fn write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    loop {
        // SAFETY: valid buffer.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Writes all of `buf` to a blocking descriptor. Usable after `fork`.
pub fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = write(fd, buf)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
    }
    Ok(())
}

/// `ftruncate`.
pub fn ftruncate(fd: BorrowedFd<'_>, len: u64) -> io::Result<()> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) }).map(drop)
}

/// `isatty`.
pub fn isatty(fd: RawFd) -> bool {
    // SAFETY: plain call.
    unsafe { libc::isatty(fd) == 1 }
}

// ---------------------------------------------------------------------------
// poll

/// Readiness interest and result for one descriptor.
#[derive(Debug, Clone, Copy)]
pub struct PollFd {
    pub fd: RawFd,
    pub read: bool,
    pub write: bool,
    pub readable: bool,
    pub writable: bool,
    /// Hang-up or error: the descriptor will not become usable again.
    pub hangup: bool,
}

impl PollFd {
    pub fn new(fd: RawFd, read: bool, write: bool) -> PollFd {
        PollFd {
            fd,
            read,
            write,
            readable: false,
            writable: false,
            hangup: false,
        }
    }
}

/// `poll` with a timeout in milliseconds (`None` waits forever). Returns
/// after `EINTR` with nothing ready.
pub fn poll(fds: &mut [PollFd], timeout_ms: Option<u64>) -> io::Result<()> {
    let mut raw: Vec<libc::pollfd> = fds
        .iter()
        .map(|p| libc::pollfd {
            fd: p.fd,
            events: (if p.read { libc::POLLIN } else { 0 })
                | (if p.write { libc::POLLOUT } else { 0 }),
            revents: 0,
        })
        .collect();
    let timeout = timeout_ms.map_or(-1, |t| t.min(i32::MAX as u64) as c_int);
    // SAFETY: raw is a valid array of its length.
    let ret = unsafe { libc::poll(raw.as_mut_ptr(), raw.len() as libc::nfds_t, timeout) };
    if ret == -1 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(err);
    }
    for (p, r) in fds.iter_mut().zip(&raw) {
        p.readable = r.revents & libc::POLLIN != 0;
        p.writable = r.revents & libc::POLLOUT != 0;
        p.hangup = r.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Processes

/// Which side of a `fork` this is.
pub enum Fork {
    Child,
    Parent(i32),
}

/// `fork`.
///
/// # Safety
///
/// The caller must be single-threaded, or the child must call only
/// async-signal-safe functions before `exec` or `_exit`.
pub unsafe fn fork() -> io::Result<Fork> {
    // SAFETY: the caller upholds the contract above.
    match cvt(unsafe { libc::fork() })? {
        0 => Ok(Fork::Child),
        pid => Ok(Fork::Parent(pid)),
    }
}

/// `setsid`. Usable after `fork`.
pub fn setsid() -> io::Result<i32> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::setsid() })
}

/// `_exit`. Usable after `fork`.
pub fn exit_now(code: i32) -> ! {
    // SAFETY: ends the process without running destructors or atexit.
    unsafe { libc::_exit(code) }
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStatus {
    Exited(i32),
    Signaled(i32),
}

/// `waitpid` for `pid` (or any child with -1). With `nohang`, `Ok(None)`
/// means it has not ended. Stops and continues are skipped.
pub fn waitpid(pid: i32, nohang: bool) -> io::Result<Option<(i32, WaitStatus)>> {
    loop {
        let mut status: c_int = 0;
        let flags = if nohang { libc::WNOHANG } else { 0 };
        // SAFETY: valid pointer.
        let ret = cvt_retry(|| unsafe { libc::waitpid(pid, &raw mut status, flags) })?;
        if ret == 0 {
            return Ok(None);
        }
        if libc::WIFEXITED(status) {
            return Ok(Some((ret, WaitStatus::Exited(libc::WEXITSTATUS(status)))));
        }
        if libc::WIFSIGNALED(status) {
            return Ok(Some((ret, WaitStatus::Signaled(libc::WTERMSIG(status)))));
        }
    }
}

/// `kill`. Signal 0 checks existence and permission.
pub fn kill(pid: i32, sig: i32) -> io::Result<()> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::kill(pid, sig) }).map(drop)
}

/// Signal numbers Perch uses.
pub mod sig {
    pub const HUP: i32 = libc::SIGHUP;
    pub const INT: i32 = libc::SIGINT;
    pub const KILL: i32 = libc::SIGKILL;
    pub const TERM: i32 = libc::SIGTERM;
    pub const CONT: i32 = libc::SIGCONT;
    pub const STOP: i32 = libc::SIGSTOP;
    pub const CHLD: i32 = libc::SIGCHLD;
    pub const PIPE: i32 = libc::SIGPIPE;
    pub const USR1: i32 = libc::SIGUSR1;
    pub const QUIT: i32 = libc::SIGQUIT;
}

/// The name of a signal, such as `SIGKILL`.
pub fn signal_name(sig: i32) -> Option<&'static str> {
    const NAMES: &[(c_int, &str)] = &[
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGCHLD, "SIGCHLD"),
        (libc::SIGCONT, "SIGCONT"),
        (libc::SIGSTOP, "SIGSTOP"),
        (libc::SIGTSTP, "SIGTSTP"),
        (libc::SIGTTIN, "SIGTTIN"),
        (libc::SIGTTOU, "SIGTTOU"),
        (libc::SIGURG, "SIGURG"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
        (libc::SIGVTALRM, "SIGVTALRM"),
        (libc::SIGPROF, "SIGPROF"),
        (libc::SIGWINCH, "SIGWINCH"),
        (libc::SIGIO, "SIGIO"),
        (libc::SIGSYS, "SIGSYS"),
    ];
    NAMES.iter().find(|(n, _)| *n == sig).map(|(_, s)| *s)
}

/// Builds an `execve` argument or environment vector: the strings and a
/// null-terminated array of pointers into them, ready before `fork`.
pub struct CVec {
    _strings: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl CVec {
    pub fn new<I, S>(items: I) -> io::Result<CVec>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let strings = items
            .into_iter()
            .map(|s| cstr(s.as_ref()))
            .collect::<io::Result<Vec<_>>>()?;
        let mut ptrs: Vec<*const c_char> = strings.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        Ok(CVec {
            _strings: strings,
            ptrs,
        })
    }
}

/// `execve`. Returns only on failure, with `errno`. Usable after `fork`.
pub fn execve(path: &CStr, argv: &CVec, envp: &CVec) -> i32 {
    // SAFETY: null-terminated arrays of valid C strings.
    unsafe { libc::execve(path.as_ptr(), argv.ptrs.as_ptr(), envp.ptrs.as_ptr()) };
    errno()
}

/// A C string from a path, for building `exec` candidates before `fork`.
pub fn path_cstring(path: &Path) -> io::Result<CString> {
    cstr(path.as_os_str())
}

/// Error codes from `exec` that mean "try the next directory on `PATH`".
pub fn exec_error_means_not_here(err: i32) -> bool {
    matches!(
        err,
        libc::ENOENT | libc::ENOTDIR | libc::ESTALE | libc::ELOOP | libc::ENAMETOOLONG
    )
}

/// `EACCES`, which `exec` keeps over a later `ENOENT` like `execvp` does.
pub fn exec_error_is_access(err: i32) -> bool {
    err == libc::EACCES
}

// ---------------------------------------------------------------------------
// Signals

static SELF_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn self_pipe_handler(sig: c_int) {
    let fd = SELF_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        let saved = errno_location_get();
        // SAFETY: write is async-signal-safe; the pipe is non-blocking.
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
        errno_location_set(saved);
    }
}

#[cfg(target_os = "linux")]
fn errno_ptr() -> *mut c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn errno_ptr() -> *mut c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__error() }
}

fn errno_location_get() -> c_int {
    // SAFETY: errno_ptr is valid.
    unsafe { *errno_ptr() }
}

fn errno_location_set(v: c_int) {
    // SAFETY: errno_ptr is valid.
    unsafe { *errno_ptr() = v };
}

fn set_disposition(sig: c_int, handler: libc::sighandler_t) -> io::Result<()> {
    // SAFETY: a zeroed sigaction is valid; we fill the fields we use.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler;
        libc::sigemptyset(&raw mut sa.sa_mask);
        sa.sa_flags = libc::SA_RESTART;
        cvt(libc::sigaction(sig, &raw const sa, std::ptr::null_mut())).map(drop)
    }
}

/// Ignores a signal. Usable after `fork`.
pub fn ignore_signal(sig: i32) -> io::Result<()> {
    set_disposition(sig, libc::SIG_IGN)
}

/// Routes the given signals to a byte each on `write_fd` (the self-pipe
/// trick). `write_fd` must be non-blocking and stay open.
pub fn route_signals_to_pipe(sigs: &[i32], write_fd: RawFd) -> io::Result<()> {
    SELF_PIPE.store(write_fd, Ordering::Relaxed);
    for &s in sigs {
        set_disposition(s, self_pipe_handler as *const () as libc::sighandler_t)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const NSIG: c_int = 65;
#[cfg(target_os = "macos")]
const NSIG: c_int = 32;

/// Restores the default disposition of every signal and clears the signal
/// mask. Usable after `fork`.
pub fn reset_signals() {
    for s in 1..NSIG {
        if s == libc::SIGKILL || s == libc::SIGSTOP {
            continue;
        }
        // Some numbers (glibc's internal real-time signals) refuse; fine.
        let _ = set_disposition(s, libc::SIG_DFL);
    }
    // SAFETY: an empty set is valid.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const set, std::ptr::null_mut());
    }
}

/// Blocks the given signals in this thread's mask (for tests and test
/// programs).
pub fn block_signals(sigs: &[i32]) {
    // SAFETY: a valid set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for &s in sigs {
            libc::sigaddset(&raw mut set, s);
        }
        libc::sigprocmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
    }
}

/// The signals currently blocked.
pub fn blocked_signals() -> Vec<i32> {
    // SAFETY: valid set pointers.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut set);
        (1..NSIG)
            .filter(|&s| libc::sigismember(&raw const set, s) == 1)
            .collect()
    }
}

/// The signals whose disposition is not the default.
pub fn non_default_signals() -> Vec<i32> {
    (1..NSIG)
        .filter(|&s| {
            // SAFETY: querying with a null new action.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                libc::sigaction(s, std::ptr::null(), &raw mut old) == 0
                    && old.sa_sigaction != libc::SIG_DFL
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Terminals

/// A window size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
}

/// `TIOCGWINSZ`.
pub fn get_winsize(fd: RawFd) -> Option<Winsize> {
    // SAFETY: a zeroed winsize is valid and the ioctl fills it.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ as _, &raw mut ws) == -1 {
            return None;
        }
        Some(Winsize {
            rows: ws.ws_row,
            cols: ws.ws_col,
        })
    }
}

/// `TIOCSWINSZ`.
pub fn set_winsize(fd: RawFd, size: Winsize) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: valid pointer.
    cvt(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &raw const ws) }).map(drop)
}

/// Terminal attributes.
#[derive(Clone, Copy)]
pub struct Termios(libc::termios);

impl Termios {
    /// Whether `IUTF8` is set.
    pub fn iutf8(&self) -> bool {
        self.0.c_iflag & libc::IUTF8 != 0
    }

    /// Sets or clears `IUTF8`.
    pub fn set_iutf8(&mut self, on: bool) {
        if on {
            self.0.c_iflag |= libc::IUTF8;
        } else {
            self.0.c_iflag &= !libc::IUTF8;
        }
    }

    /// Whether `ECHO` is set.
    pub fn echo(&self) -> bool {
        self.0.c_lflag & libc::ECHO != 0
    }

    /// Sets or clears `ECHO`.
    pub fn set_echo(&mut self, on: bool) {
        if on {
            self.0.c_lflag |= libc::ECHO;
        } else {
            self.0.c_lflag &= !libc::ECHO;
        }
    }

    /// Whether `ICANON` is set.
    pub fn icanon(&self) -> bool {
        self.0.c_lflag & libc::ICANON != 0
    }
}

/// `tcgetattr`.
pub fn tcgetattr(fd: RawFd) -> Option<Termios> {
    // SAFETY: a zeroed termios is valid and tcgetattr fills it.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(fd, &raw mut t) == 0).then_some(Termios(t))
    }
}

/// `tcsetattr` with `TCSANOW`.
pub fn tcsetattr(fd: RawFd, t: &Termios) -> io::Result<()> {
    // SAFETY: valid pointer.
    cvt(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const t.0) }).map(drop)
}

/// A new pseudo-terminal: the master and the slave's path, both
/// close-on-exec. The master is not a controlling terminal.
pub fn open_pty() -> io::Result<(OwnedFd, std::path::PathBuf)> {
    // SAFETY: plain calls; ptsname's static buffer is copied at once and
    // the process is single-threaded where this is used.
    unsafe {
        let fd = cvt(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY))?;
        let master = OwnedFd::from_raw_fd(fd);
        set_cloexec(fd, true)?;
        cvt(libc::grantpt(fd))?;
        cvt(libc::unlockpt(fd))?;
        let name = libc::ptsname(fd);
        if name.is_null() {
            return Err(io::Error::last_os_error());
        }
        let path = std::path::PathBuf::from(OsStr::from_bytes(CStr::from_ptr(name).to_bytes()));
        Ok((master, path))
    }
}

/// Makes `fd` the controlling terminal of this (new) session. Usable after
/// `fork`.
pub fn set_controlling_tty(fd: RawFd) -> io::Result<()> {
    // SAFETY: plain ioctl.
    cvt(unsafe { libc::ioctl(fd, libc::TIOCSCTTY as _, 0) }).map(drop)
}

// ---------------------------------------------------------------------------
// Sockets

/// The uid of the peer of a connected Unix socket.
pub fn peer_uid(fd: BorrowedFd<'_>) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a zeroed ucred is valid and getsockopt fills it.
        unsafe {
            let mut cred: libc::ucred = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            cvt(libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut cred).cast(),
                &raw mut len,
            ))?;
            Ok(cred.uid)
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: valid pointers.
        cvt(unsafe { libc::getpeereid(fd.as_raw_fd(), &raw mut uid, &raw mut gid) })?;
        Ok(uid)
    }
}

// ---------------------------------------------------------------------------
// Process information (macOS; Linux reads /proc in procinfo.rs)

/// What `proc_pidinfo(PROC_PIDTBSDINFO)` says about a process.
#[cfg(target_os = "macos")]
pub struct BsdInfo {
    pub ruid: u32,
    pub start_sec: u64,
    pub start_usec: u64,
    pub zombie: bool,
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)`.
#[cfg(target_os = "macos")]
pub fn proc_bsd_info(pid: i32) -> Option<BsdInfo> {
    // From <sys/proc.h>.
    const SZOMB: u32 = 5;
    // SAFETY: a zeroed proc_bsdinfo is valid; the call fills it.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as c_int;
        let n = libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size);
        (n == size).then_some(BsdInfo {
            ruid: info.pbi_ruid,
            start_sec: info.pbi_start_tvsec,
            start_usec: info.pbi_start_tvusec,
            zombie: info.pbi_status == SZOMB,
        })
    }
}

/// Every process ID on the system.
#[cfg(target_os = "macos")]
pub fn list_all_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count; then a buffer of that size.
    unsafe {
        let n = libc::proc_listallpids(std::ptr::null_mut(), 0);
        if n <= 0 {
            return Vec::new();
        }
        let mut pids = vec![0i32; n as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<i32>()) as c_int;
        let got = libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes);
        if got <= 0 {
            return Vec::new();
        }
        pids.truncate(got as usize);
        pids.retain(|&p| p > 0);
        pids
    }
}
