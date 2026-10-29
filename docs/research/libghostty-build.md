# libghostty-vt build check (2026-10-29)

A check that libghostty-vt builds and behaves as the design needs, run on macOS 26.7 (arm64, SDK 26.5) with Rust 1.99.

## Building

| Bindings | Ghostty commit | Zig | Result |
|----------|----------------|-----|--------|
| crates.io `libghostty-vt` 0.2.2 | `a887df4` | 0.15.2 | Fails: Zig 0.15.2 cannot link its own build runner against the macOS 26 SDK (`undefined symbol: _sigaction`, `_waitpid`, ...) |
| git `uzaaft/libghostty-rs` at `9ede35b` | `521cf2c` | 0.17.0 | Builds in about 75 seconds from clean |

So Perch pins the bindings by git commit, not the crates.io release, until a release carries a Ghostty commit that builds with Zig 0.17.

## Behavior

Probe program (Rust, release build):

- `ESC[6n` after writing `hello\r\nab` produces `ESC[2;3R` through `on_pty_write`, at the query, before the following output. `ESC[5n` produces `ESC[0n`. `ESC[c` produces `ESC[?62;22c` unless the DA callback overrides it.
- The VT formatter with modes and cursor extras writes `ESC[?1000h ESC[?2004h`, the content, and `ESC[2;5H`.
- 10,000 lines of R-like output at 120 columns: about 9.8 MB of resident memory once the byte limit is removed (`set_scrollback_max_bytes(None)`). With only the line limit set, a default byte limit kept 457 rows. The line limit is approximate, to page granularity.
- Throughput on mixed SGR text: about 500 to 600 MB/s.

## A Zig library over Ghostty's modules

Design revision 9 uses a Zig library of Perch's own (`perch-vt`) over Ghostty's internal `ghostty-vt` module instead of the C interface. Check, with Zig 0.17.0 and Ghostty `521cf2c` fetched by `zig fetch --save` (hash `ghostty-1.3.2-dev-5UdBC6WZfwXgQ6HmA5DH6dGNAUqtDMLrhhrHiBX-99J0`):

- A Zig program importing `ghostty-vt` builds and runs on macOS 26.
- After `line one`, `line two`, `$ vim` and then `ESC[?1049h ESC[2J ESC[H ~ vim screen ~`, the active screen is the alternate one, and `ScreenFormatter` on `screens.get(.primary)` with cursor and style extras writes `line one\r\nline two\r\n$ vim ESC[4;1H ESC[0m`. The C interface cannot format the inactive screen; this can.
- `ScreenFormatter.Content` has a `none` case for state without content, and `cursor_origin` handles origin mode.

## SIMD and cross-compiling

Same probe, extended to time parsing of mixed SGR and non-ASCII text, built `ReleaseSafe`:

- `-Dsimd=true`: 354 MB/s. `-Dsimd=false`: 400 MB/s (macOS arm64).
- With `-Dsimd=false`, `zig build -Dtarget=` produced a static x86-64 musl ELF, a static arm64 musl ELF and an x86-64 Mach-O. The arm64 musl build ran on `rockylinux:8` and `alpine:3` (about 290 MB/s under colima).
- Not yet checked: linking a `perch-vt` static library into a Rust binary for each target.

## Model memory at the history and width limits (M2c)

Measured 2026-10-29 on macOS 26 (arm64) with `perch-testprogs model-dump --memory LINES COLS`: one `perch-vt` model (ReleaseSafe, SIMD off) at 50 rows, scrollback set to LINES lines and Perch's 64 MiB byte limit, fed LINES + 50 lines that fill every cell with a color change every 8 cells. The figure is the process's peak resident memory (`getrusage`), measured before anything else is allocated, so it includes about 2.4 MB for the process itself.

| History lines | Columns | Rows kept in history | Peak resident memory |
|---------------|---------|----------------------|----------------------|
| 1,000 | 120 | 605 | 6.3 MB |
| 1,000 | 500 | 906 | 11.2 MB |
| 10,000 | 120 | 9,605 | 18.7 MB |
| 10,000 | 500 | 9,906 | 62.1 MB |

History is pruned by whole pages, so fewer rows are kept than the line limit, more so at narrow widths where a page holds more rows. `crates/perch/tests/model_memory.rs` runs the largest case on every CI run and fails above 120 MB.
