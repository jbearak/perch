// SPDX-License-Identifier: GPL-3.0-only

//! Every call into `perch-vt`, the Zig library over Ghostty's terminal
//! modules. The C declarations here mirror `perch-vt/src/main.zig` by hand.

use std::ffi::c_int;

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

unsafe extern "C" {
    fn perch_vt_selftest(out: *mut RawSelftest) -> c_int;
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

    #[test]
    fn selftest_passes() {
        let result = selftest().expect("perch_vt_selftest status");
        assert_eq!(result.dsr6_reply, b"\x1b[2;1R");
        assert_eq!(result.cursor, (0, 1));
        assert!(!result.kitty_graphics);
        assert!(result.ok(), "{result:?}");
    }
}
