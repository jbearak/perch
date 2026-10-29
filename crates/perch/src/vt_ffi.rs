// SPDX-License-Identifier: GPL-3.0-only

//! Every call into `perch-vt`, the Zig library over Ghostty's terminal
//! modules. The C declarations here mirror `perch-vt/src/main.zig` by hand.

use std::ffi::{CStr, c_char, c_int};
use std::ptr::NonNull;

/// Mirrors `Selftest` in `perch-vt/src/main.zig`.
#[repr(C)]
struct RawSelftest {
    cursor_col: u32,
    cursor_row: u32,
    cols: u32,
    rows: u32,
    kitty_graphics: u32,
    reply_len: u32,
    formatted_len: u32,
    reply: [u8; 32],
    formatted: [u8; 256],
}

/// A `perch-vt` model, opaque to Rust.
#[repr(C)]
struct RawVt {
    _private: [u8; 0],
}

/// Mirrors `Bytes` in `perch-vt/src/main.zig`: memory `perch-vt` allocated,
/// freed with `perch_vt_bytes_free`.
#[repr(C)]
struct RawBytes {
    ptr: *mut u8,
    len: usize,
}

/// Mirrors `modes.CEntry` in `perch-vt/src/modes.zig`.
#[repr(C)]
struct RawModeEntry {
    value: u16,
    ansi: u8,
    default: u8,
    prologue_reset: u8,
    replay: u8,
    reason: *const c_char,
}

unsafe extern "C" {
    fn perch_vt_selftest(out: *mut RawSelftest) -> c_int;
    fn perch_vt_new(
        cols: u16,
        rows: u16,
        scrollback_lines: u32,
        scrollback_bytes: u64,
    ) -> *mut RawVt;
    fn perch_vt_free(vt: *mut RawVt);
    fn perch_vt_feed(vt: *mut RawVt, bytes: *const u8, len: usize, frame: u64);
    fn perch_vt_resize(vt: *mut RawVt, cols: u16, rows: u16) -> c_int;
    fn perch_vt_at_ground(vt: *mut RawVt) -> bool;
    fn perch_vt_take_replies(vt: *mut RawVt, out: *mut Dsr6Reply, cap: usize) -> usize;
    fn perch_vt_restore(
        vt: *mut RawVt,
        mode: u32,
        client_cols: u16,
        client_rows: u16,
        client_scrollback_rows: u32,
        history_cap: usize,
        byte_cap: usize,
        out: *mut RawBytes,
    ) -> c_int;
    fn perch_vt_tail(vt: *mut RawVt, history_rows: u32, out: *mut RawBytes) -> c_int;
    fn perch_vt_bytes_free(out: *mut RawBytes);
    fn perch_vt_title(vt: *mut RawVt, len: *mut usize) -> *const u8;
    fn perch_vt_pwd(vt: *mut RawVt, len: *mut usize) -> *const u8;
    fn perch_vt_mode(vt: *mut RawVt, value: u16, ansi: bool) -> c_int;
    fn perch_vt_active_screen(vt: *mut RawVt) -> c_int;
    fn perch_vt_prologue_table(len: *mut usize) -> *const RawModeEntry;
    #[cfg(feature = "test-hooks")]
    fn perch_vt_take_observed(vt: *mut RawVt, out: *mut Observed, cap: usize) -> usize;
}

/// A failure `perch-vt` reported, from its negative `Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtError {
    OutOfMemory,
    /// The restore would pass its byte limit.
    TooLarge,
    InvalidArgument,
    Other(i32),
}

impl VtError {
    fn check(status: c_int) -> Result<(), VtError> {
        match status {
            0 => Ok(()),
            -1 => Err(VtError::OutOfMemory),
            -4 => Err(VtError::TooLarge),
            -5 => Err(VtError::InvalidArgument),
            other => Err(VtError::Other(other)),
        }
    }
}

/// A DSR 6 reply from the model: the cursor position it reported,
/// one-based, and the frame whose bytes held the query. Mirrors `Reply` in
/// `perch-vt/src/Vt.zig`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dsr6Reply {
    pub frame: u64,
    pub row: u32,
    pub col: u32,
}

impl Dsr6Reply {
    /// The reply as the terminal sends it: `ESC [ row ; col R`.
    pub fn bytes(&self) -> Vec<u8> {
        format!("\x1b[{};{}R", self.row, self.col).into_bytes()
    }
}

/// A query the test observer saw: its kind and the number of bytes fed up
/// to and including its last byte, counted from the model's creation.
/// Mirrors `Observed` in `perch-vt/src/Vt.zig`.
#[cfg(feature = "test-hooks")]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Observed {
    pub kind: u32,
    _pad: u32,
    pub end: u64,
}

#[cfg(feature = "test-hooks")]
impl Observed {
    pub const DSR5: u32 = 1;
    pub const DSR6: u32 = 2;
    pub const DA1: u32 = 3;
    pub const DA2: u32 = 4;
    pub const PASTE_SET: u32 = 5;
    pub const PASTE_RESET: u32 = 6;
    pub const PASTE_SAVE: u32 = 7;
    pub const PASTE_RESTORE: u32 = 8;
}

/// Which restore to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// History, then the screens and terminal state.
    Full = 0,
    /// The screens and terminal state, without history.
    Screen = 1,
}

/// The limits on a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreLimits {
    /// The most history rows the client keeps, at its width.
    pub client_scrollback_rows: u32,
    /// The most bytes of history; the newest rows that fit are kept.
    pub history_cap: usize,
    /// The most bytes for everything else.
    pub byte_cap: usize,
}

/// The screen the program is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveScreen {
    Primary,
    Alternate,
}

/// Whether and when a restore replays a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replay {
    No,
    /// With the other terminal state, after the screens.
    Yes,
    /// Before any history or screen content.
    Early,
}

/// One row of `perch-vt`'s mode table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeEntry {
    pub value: u16,
    /// An ANSI mode (`CSI n h`) rather than a DEC private one (`CSI ? n h`).
    pub ansi: bool,
    /// Ghostty's default.
    pub default: bool,
    /// Whether the prologue sets the mode to its default.
    pub prologue_reset: bool,
    pub replay: Replay,
    /// Why the mode is not replayed.
    pub reason: Option<&'static str>,
}

/// Every mode Ghostty knows, with its default and Perch's marks.
pub fn mode_table() -> Vec<ModeEntry> {
    let mut len = 0;
    // SAFETY: the table is static data in perch-vt; `len` is valid to write.
    let ptr = unsafe { perch_vt_prologue_table(&mut len) };
    // SAFETY: perch-vt returns a pointer to `len` entries that live as long
    // as the program.
    let raw = unsafe { std::slice::from_raw_parts(ptr, len) };
    raw.iter()
        .map(|e| ModeEntry {
            value: e.value,
            ansi: e.ansi != 0,
            default: e.default != 0,
            prologue_reset: e.prologue_reset != 0,
            replay: match e.replay {
                0 => Replay::No,
                2 => Replay::Early,
                _ => Replay::Yes,
            },
            reason: (!e.reason.is_null()).then(|| {
                // SAFETY: a non-null reason is a static NUL-terminated
                // string in perch-vt.
                unsafe { CStr::from_ptr(e.reason) }
                    .to_str()
                    .expect("perch-vt mode reasons are UTF-8")
            }),
        })
        .collect()
}

/// A terminal model: Ghostty's terminal fed the program's output.
pub struct Model {
    raw: NonNull<RawVt>,
}

impl Model {
    /// A model of `cols` by `rows` cells (each at least 1) whose history is
    /// limited by lines and by bytes, whichever binds first.
    pub fn new(
        cols: u16,
        rows: u16,
        scrollback_lines: u32,
        scrollback_bytes: u64,
    ) -> Result<Model, VtError> {
        // SAFETY: plain values in; a null result means out of memory.
        let raw = unsafe { perch_vt_new(cols, rows, scrollback_lines, scrollback_bytes) };
        NonNull::new(raw)
            .map(|raw| Model { raw })
            .ok_or(VtError::OutOfMemory)
    }

    fn ptr(&self) -> *mut RawVt {
        self.raw.as_ptr()
    }

    /// Feed output belonging to `frame`; DSR 6 replies to queries in it
    /// carry that frame.
    pub fn feed(&mut self, bytes: &[u8], frame: u64) {
        // SAFETY: the model is live and `bytes` is valid for its length.
        unsafe { perch_vt_feed(self.ptr(), bytes.as_ptr(), bytes.len(), frame) }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), VtError> {
        // SAFETY: the model is live.
        VtError::check(unsafe { perch_vt_resize(self.ptr(), cols, rows) })
    }

    /// Whether the parser and its UTF-8 decoder are between sequences.
    pub fn at_ground(&self) -> bool {
        // SAFETY: the model is live.
        unsafe { perch_vt_at_ground(self.ptr()) }
    }

    /// The DSR 6 replies produced since the last call, oldest first.
    pub fn take_replies(&mut self) -> Vec<Dsr6Reply> {
        let mut out = Vec::new();
        let mut buf = [Dsr6Reply::default(); 32];
        loop {
            // SAFETY: the model is live and `buf` holds `buf.len()` entries.
            let n = unsafe { perch_vt_take_replies(self.ptr(), buf.as_mut_ptr(), buf.len()) };
            out.extend_from_slice(&buf[..n.min(buf.len())]);
            if n < buf.len() {
                return out;
            }
        }
    }

    /// Build a restore for a client of `cols` by `rows`. The model is
    /// resized to that size first if it differs; resizing the PTY is the
    /// caller's. The prologue, and for a full restore the clear before it,
    /// are the caller's too.
    pub fn restore(
        &mut self,
        mode: RestoreMode,
        cols: u16,
        rows: u16,
        limits: RestoreLimits,
    ) -> Result<Vec<u8>, VtError> {
        let mut out = RawBytes {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        // SAFETY: the model is live and `out` is valid to write.
        let status = unsafe {
            perch_vt_restore(
                self.ptr(),
                mode as u32,
                cols,
                rows,
                limits.client_scrollback_rows,
                limits.history_cap,
                limits.byte_cap,
                &mut out,
            )
        };
        VtError::check(status)?;
        Ok(take_bytes(out))
    }

    /// The last `history_rows` rows of history and the active screen as
    /// text with SGR, for `tail.txt`.
    pub fn tail(&self, history_rows: u32) -> Result<Vec<u8>, VtError> {
        let mut out = RawBytes {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        // SAFETY: the model is live and `out` is valid to write.
        VtError::check(unsafe { perch_vt_tail(self.ptr(), history_rows, &mut out) })?;
        Ok(take_bytes(out))
    }

    /// The window title the program set, if any.
    pub fn title(&self) -> Option<Vec<u8>> {
        let mut len = 0;
        // SAFETY: the model is live; the result is valid until the next
        // feed, and is copied before this returns.
        let ptr = unsafe { perch_vt_title(self.ptr(), &mut len) };
        borrowed(ptr, len)
    }

    /// The working directory the program reported with OSC 7, if any.
    pub fn pwd(&self) -> Option<Vec<u8>> {
        let mut len = 0;
        // SAFETY: as for `title`.
        let ptr = unsafe { perch_vt_pwd(self.ptr(), &mut len) };
        borrowed(ptr, len)
    }

    /// Whether a mode is set, or `None` if Ghostty does not know it.
    pub fn mode(&self, value: u16, ansi: bool) -> Option<bool> {
        // SAFETY: the model is live.
        match unsafe { perch_vt_mode(self.ptr(), value, ansi) } {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    pub fn active_screen(&self) -> ActiveScreen {
        // SAFETY: the model is live.
        match unsafe { perch_vt_active_screen(self.ptr()) } {
            0 => ActiveScreen::Primary,
            _ => ActiveScreen::Alternate,
        }
    }

    /// The queries the parser dispatched since the last call, oldest first.
    #[cfg(feature = "test-hooks")]
    pub fn take_observed(&mut self) -> Vec<Observed> {
        let mut out = Vec::new();
        let mut buf = [Observed::default(); 32];
        loop {
            // SAFETY: the model is live and `buf` holds `buf.len()` entries.
            let n = unsafe { perch_vt_take_observed(self.ptr(), buf.as_mut_ptr(), buf.len()) };
            out.extend_from_slice(&buf[..n.min(buf.len())]);
            if n < buf.len() {
                return out;
            }
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: the model is live and freed exactly once.
        unsafe { perch_vt_free(self.ptr()) }
    }
}

/// Copy bytes `perch-vt` allocated and free them.
fn take_bytes(mut raw: RawBytes) -> Vec<u8> {
    let bytes = if raw.ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: perch-vt returned `len` initialized bytes at `ptr`.
        unsafe { std::slice::from_raw_parts(raw.ptr, raw.len) }.to_vec()
    };
    // SAFETY: `raw` came from perch-vt and is freed once.
    unsafe { perch_vt_bytes_free(&mut raw) };
    bytes
}

fn borrowed(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: perch-vt returned `len` bytes at `ptr`, valid until the next
    // feed, which cannot happen while `&self` is borrowed.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

/// What `perch_vt_selftest` saw after feeding `ab\r\n` and `ESC[6n` to an
/// 80x24 terminal and resizing it to 100x30.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selftest {
    /// Zero-based cursor column and row after the resize.
    pub cursor: (u32, u32),
    /// Columns and rows after the resize.
    pub size: (u32, u32),
    /// Whether Ghostty's kitty graphics support was compiled in.
    pub kitty_graphics: bool,
    /// The DSR 6 reply the terminal wrote back.
    pub dsr6_reply: Vec<u8>,
    /// The active screen formatted as VT with the cursor.
    pub formatted: Vec<u8>,
}

impl Selftest {
    /// Whether the terminal behaved as Ghostty does: one DSR 6 reply for row
    /// 2, column 1, the cursor kept there through the resize, the text kept,
    /// and kitty graphics compiled out.
    pub fn ok(&self) -> bool {
        self.cursor == (0, 1)
            && self.size == (100, 30)
            && !self.kitty_graphics
            && self.dsr6_reply == b"\x1b[2;1R"
            && self.formatted.starts_with(b"ab")
    }
}

/// Run `perch_vt_selftest`. Errors carry the library's negative status.
pub fn selftest() -> Result<Selftest, i32> {
    let mut raw = RawSelftest {
        cursor_col: 0,
        cursor_row: 0,
        cols: 0,
        rows: 0,
        kitty_graphics: 0,
        reply_len: 0,
        formatted_len: 0,
        reply: [0; 32],
        formatted: [0; 256],
    };
    // SAFETY: `raw` is a valid, writable `RawSelftest` for the whole call,
    // and the library writes nothing else.
    let status = unsafe { perch_vt_selftest(&mut raw) };
    if status != 0 {
        return Err(status);
    }
    let reply_len = (raw.reply_len as usize).min(raw.reply.len());
    let formatted_len = (raw.formatted_len as usize).min(raw.formatted.len());
    Ok(Selftest {
        cursor: (raw.cursor_col, raw.cursor_row),
        size: (raw.cols, raw.rows),
        kitty_graphics: raw.kitty_graphics != 0,
        dsr6_reply: raw.reply[..reply_len].to_vec(),
        formatted: raw.formatted[..formatted_len].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RestoreLimits = RestoreLimits {
        client_scrollback_rows: 1000,
        history_cap: 4 << 20,
        byte_cap: 32 << 20,
    };

    #[test]
    fn restore_rebuilds_the_screen_in_a_second_model() {
        let mut a = Model::new(20, 4, 1000, 1 << 20).unwrap();
        a.feed(
            b"one\r\ntwo\r\nthree\r\nfour\r\n\x1b[1mbold\x1b]2;title\x07",
            0,
        );
        let bytes = a.restore(RestoreMode::Full, 20, 4, LIMITS).unwrap();
        assert!(bytes.windows(3).any(|w| w == b"one"), "{bytes:?}");

        let mut b = Model::new(20, 4, 1000, 1 << 20).unwrap();
        b.feed(&bytes, 0);
        assert_eq!(b.title().as_deref(), Some(&b"title"[..]));
        // Both screens agree on where the cursor is.
        a.feed(b"\x1b[6n", 1);
        b.feed(b"\x1b[6n", 1);
        assert_eq!(a.take_replies(), b.take_replies());

        // A screen restore leaves the history out.
        let screen = a.restore(RestoreMode::Screen, 20, 4, LIMITS).unwrap();
        assert!(!screen.windows(3).any(|w| w == b"one"));

        let tail = a.tail(1000).unwrap();
        assert!(tail.starts_with(b"one\n"), "{tail:?}");
    }

    #[test]
    fn restore_reports_its_byte_cap() {
        let mut a = Model::new(20, 4, 1000, 1 << 20).unwrap();
        a.feed(b"\x1b[31mred\x1b[0m", 0);
        let limits = RestoreLimits {
            byte_cap: 4,
            ..LIMITS
        };
        assert_eq!(
            a.restore(RestoreMode::Screen, 20, 4, limits),
            Err(VtError::TooLarge)
        );
    }

    #[test]
    fn only_dsr6_replies_come_back_tagged_with_frames() {
        let mut vt = Model::new(80, 24, 1000, 1 << 20).unwrap();
        vt.feed(b"ab\x1b[6n", 3);
        vt.feed(b"\x1b[5n\x1b[c\x1b[?6n\x1b[?2004$p", 4);
        vt.feed(b"\r\n\x1b[6n\x1b[", 5);
        assert!(!vt.at_ground());
        vt.feed(b"m", 6);
        assert!(vt.at_ground());
        let replies = vt.take_replies();
        assert_eq!(
            replies,
            [
                Dsr6Reply {
                    frame: 3,
                    row: 1,
                    col: 3
                },
                Dsr6Reply {
                    frame: 5,
                    row: 2,
                    col: 1
                },
            ]
        );
        assert_eq!(replies[0].bytes(), b"\x1b[1;3R");
        assert!(vt.take_replies().is_empty());
    }

    #[test]
    fn state_queries() {
        let mut vt = Model::new(80, 24, 1000, 1 << 20).unwrap();
        assert_eq!(vt.title(), None);
        vt.feed(b"\x1b]7;file://h/tmp\x07\x1b[?2004h\x1b[?1049h", 0);
        assert_eq!(vt.pwd().as_deref(), Some(&b"file://h/tmp"[..]));
        assert_eq!(vt.mode(2004, false), Some(true));
        assert_eq!(vt.mode(4, true), Some(false));
        assert_eq!(vt.mode(9999, false), None);
        assert_eq!(vt.active_screen(), ActiveScreen::Alternate);
        vt.resize(100, 30).unwrap();
    }

    #[test]
    fn mode_table_follows_the_design() {
        let table = mode_table();
        let find = |value, ansi| {
            *table
                .iter()
                .find(|e| e.value == value && e.ansi == ansi)
                .unwrap()
        };
        assert_eq!(find(2027, false).replay, Replay::Early);
        let sync = find(2026, false);
        assert!(sync.prologue_reset && sync.replay == Replay::No && sync.reason.is_some());
        for value in [3, 47, 1007, 1047, 1048, 1049, 2031, 2033, 2048, 5522] {
            let e = find(value, false);
            assert_eq!(e.replay, Replay::No, "{value}");
            assert!(e.reason.is_some(), "{value}");
        }
        assert!(find(7, false).default && find(7, false).replay == Replay::Yes);
        assert!(find(4, true).prologue_reset);
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn observer_reports_queries() {
        let mut vt = Model::new(80, 24, 1000, 1 << 20).unwrap();
        vt.feed(b"x\x1b[6n\x1b[?2004h", 0);
        let seen: Vec<_> = vt.take_observed().iter().map(|o| (o.kind, o.end)).collect();
        assert_eq!(seen, [(Observed::DSR6, 5), (Observed::PASTE_SET, 13)]);
    }

    #[test]
    fn selftest_passes() {
        let result = selftest().expect("perch_vt_selftest status");
        assert_eq!(result.dsr6_reply, b"\x1b[2;1R");
        assert_eq!(result.cursor, (0, 1));
        assert!(!result.kitty_graphics);
        assert!(result.ok(), "{result:?}");
    }
}
