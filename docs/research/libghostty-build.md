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
