# Perch implementation plan

Status: draft for review, revision 6. This plan implements [design.md](design.md) at revision 16. Where they disagree, the design wins and the plan gets fixed.

## Principles

- **Riskiest first.** `perch-vt` over Ghostty, restore, and the query and flow rules are where the design can be wrong. They are proven first: `perch-vt` with Zig tests, then the tracker against `perch-vt`, then restore against real terminal emulators, then the query, input and flow state machines as pure code with a fake clock, all before a supervisor process uses them.
- **One PR per milestone**, merged before the next starts. Each PR leaves `main` building and tested. Milestones before M5 are not release-quality on their own. Nothing is released before M6.
- **Proof in every PR.** Each milestone lists its tests. CI runs them. Anything that cannot be checked here goes in [verification.md](verification.md) with status "not run", and PR bodies say "not run", never "recorded".
- **The contract is the design's.** A fixture or test that the design requires and that cannot pass is not resolved by narrowing the contract inside an implementation PR. The PR stops, and the change goes back through design review.
- **Small dependency set.** Rust runtime dependencies: `libc`, `serde`, `serde_json`. No async runtime, no argument-parsing crate, no `nix`, no third-party Ghostty bindings. All `libc` calls live in one `sys` module and all calls into `perch-vt` in one `vt_ffi` module.
- **No runtime npm dependencies** in the extension. A CI check fails if `dependencies` in `editors/vscode/package.json` is not empty. Development and test packages are pinned by lockfile and listed with their licenses in `docs/licenses.md`, checked in CI against an allow-list.
- **Licenses.** `deny.toml` allows MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib and NCSA (which `libfuzzer-sys` carries), sets `include-dev = true`, and ignores Perch's own crates through `publish = false` and `licenses.private.ignore = true`. CI runs `cargo deny check licenses bans sources` on the workspace and again with `--manifest-path fuzz/Cargo.toml`. Advisories run weekly, not on every PR. A script checks that every source file starts with `// SPDX-License-Identifier: GPL-3.0-only` (or the Zig and TypeScript equivalents). The one exception is `tracker.rs`, a port of MIT code from Ghostty, whose header is `GPL-3.0-only AND MIT` and keeps Ghostty's copyright line.
- **Zig packages.** `perch-vt/zig-packages.toml` lists every Zig package the shipped build fetches: name, content hash, license, and whether it is linked or used only at build time. A CI step runs `zig build` the same way, with the shipped flags and empty global and local package directories, lists what lands in the local `zig-pkg/`, and fails on any package not in the file. `NOTICE` covers every linked one.
- **Pinned toolchains.** Rust stable, with `rust-version = "1.95"` in the workspace, because `reedline` 0.52.1 (a test dependency) needs it. A CI job checks the workspace with Rust 1.95. Zig 0.17.0 from ziglang.org, checked against SHA-256 digests committed in `perch-vt/zig-toolchain.toml` for the three host tarballs CI and the maintainer use (`x86_64-linux`, `aarch64-linux`, `aarch64-macos`). CI never uses macOS Intel runners: `x86_64-apple-darwin` is cross-compiled on the arm64 runner and not executed.
- **Two kinds of test build.** Plain `cargo test` covers what needs no hooks. `cargo test --features test-hooks` enables `PERCH_TEST_TIME_SCALE`, fault injection (model crash, hang, memory pressure, missed replies) and `perch-vt`'s test observer (`build.rs` passes `-Dtest-observer=true` when the feature is on), and runs every integration test and the differential tests, which are compiled only with the feature. The fuzz crate depends on `perch` with `test-hooks`. Release builds have neither, and CI checks that with `nm` and `strings` on the release binary: no `perch_vt_take_observed` symbol and no `PERCH_TEST_TIME_SCALE` string.
- **Test programs.** Integration tests that need `perch-testprogs` run after `cargo build -p perch-testprogs` with the same `--target` and the same `test-hooks` setting as the test run that follows (CI does this before each test run, because `perch-testprogs` links `perch`), and find the programs through a helper that resolves `target/<profile>/<name>` from the test binary's own location, failing with a clear message if one is missing.
- **Short socket paths in tests.** Every test sets its own `PERCH_RUNTIME_DIR` under `/tmp`.

## The build contract

`crates/perch/build.rs` builds `perch-vt` and links it. Its behavior is fixed here so no PR has to decide it:

- **Zig**: `$PERCH_ZIG` if set, else `zig` on `PATH`. The build fails with a clear message unless `zig version` prints exactly `0.17.0`.
- **Target mapping**: `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl` to Zig `x86_64-linux-musl`; `aarch64-unknown-linux-gnu` and `aarch64-unknown-linux-musl` to `aarch64-linux-musl`; `aarch64-apple-darwin` to `aarch64-macos.11.0`; `x86_64-apple-darwin` to `x86_64-macos.11.0`. `perch-vt` needs no libc of its own, so a musl Zig object links into a gnu Rust binary for tests and fuzzing. Anything else fails with a message.
- **Flags**: `-Doptimize=ReleaseSafe -Dcpu=baseline -Dsimd=false -Dvt-features=-kitty-graphics`. `-Dcpu=baseline` keeps release binaries running on old cluster CPUs. The `ghostty-vt` module takes `vt-features` too, so kitty graphics, and its image storage, are compiled out; M1's selftest confirms it.
- **The static library**: `perch-vt/build.zig` builds it as Ghostty builds its own static library: position-independent code (`pic = true`, because Rust links position-independent executables on Linux), `bundle_compiler_rt = true` and `bundle_ubsan_rt = true`. `build.rs` sets `MACOSX_DEPLOYMENT_TARGET=11.0` for the Rust side so both halves target the same macOS. The duplicate-symbol scan is `nm` on the final binary with no strong symbol defined twice.
- **Caches and outputs**: `build.rs` runs `zig build` with `$OUT_DIR` as its working directory, `--build-file <absolute path>/perch-vt/build.zig`, `--cache-dir $OUT_DIR/zig-cache`, `--prefix $OUT_DIR/zig-out`, and `ZIG_LOCAL_PKG_DIR=zig-pkg`. The package directory must be relative: Ghostty's `build.zig.zon` has path dependencies (`./pkg/zlib` and others), and Zig rejects them when the local package directory is absolute. Relative to the working directory, Zig 0.17 unpacks packages into `$OUT_DIR/zig-pkg`, and nothing is written inside `perch-vt/` (a review confirmed this form on the pinned Ghostty). The global cache (`$ZIG_GLOBAL_CACHE_DIR` if set, else Zig's default) holds the package tarballs. CI sets it and caches it, keyed by the hash of `build.zig.zon`.
- **Rebuilds**: `cargo:rerun-if-changed` for everything under `perch-vt/` (which the build never writes to) and `cargo:rerun-if-env-changed` for `PERCH_ZIG`, `ZIG_GLOBAL_CACHE_DIR` and the `test-hooks` feature.
- **Network**: Zig fetches Ghostty and its packages on the first build. CI's package cache keeps builds working when the package hosts are slow or down.

Local checks of the musl and darwin-x64 builds are not possible with this laptop's Homebrew Rust, which has no standard library for those targets. `tests/linux/rust.Dockerfile` (Ubuntu with rustup, the musl targets and Zig) is the local route for Linux targets. darwin-x64 is checked only by CI's cross-build.

## Repository layout

```text
Cargo.toml                       workspace; rust-version = "1.95"; release profile keeps panic = "unwind"
deny.toml, NOTICE
crates/perch/                    library (src/lib.rs) and binary (src/main.rs)
  build.rs                       the build contract above
  src/sys.rs, src/vt_ffi.rs, src/tracker.rs
  src/runtime.rs, src/meta.rs, src/proto.rs
  src/flow.rs, src/input.rs, src/queries.rs   pure state machines
  src/model/                     model process and the supervisor's side of it
  src/restore.rs                 prologue, epilogue, restore assembly
  src/supervisor/, src/client.rs, src/env_links.rs, src/procinfo.rs, src/selfinstall.rs
  src/cli/
  tests/
crates/perch-testprogs/          dsr-timer, stdin-recorder, reedline-prompt, model-dump, exit-with, fd-lister
perch-vt/                        Zig library: build.zig, build.zig.zon, zig-packages.toml, zig-toolchain.toml, src/, tests
fuzz/                            cargo-fuzz crate, outside the workspace
tests/fidelity/                  Node harness (xterm-headless 5.3.0-beta.61, @xterm/headless 5.5.0 and 6.0.0, each with its Unicode 11 addon)
tests/linux/                     Dockerfiles and scripts for host checks
scripts/                         spdx-check, zig-package-check
editors/vscode/                  extension: src/core/ (plain logic), src/vscode/ (adapter)
docs/
.github/workflows/               ci.yml, fuzz.yml, advisories.yml, release.yml
```

## Milestones

### M0. Design and plan

Branch `docs/design`: `docs/design.md`, `docs/plan.md`, `docs/research/`, `docs/verification.md`, README links. The design's review history records its reviews by Sonnet and Terra; this plan's review history records its reviews by Sonnet and Sol.

### M1. Skeleton, toolchains and a real link

- Workspace with `crates/perch` and `crates/perch-testprogs`. `perch-vt/` with Ghostty pinned at `521cf2c` in `build.zig.zon`, `zig-toolchain.toml`, and `zig-packages.toml` filled from the first real build.
- **A real link.** `perch-vt` exports `perch_vt_selftest`, which creates a Ghostty terminal, feeds `ab\r\n` and `ESC[6n`, resizes it, formats its screen, frees it, and returns the cursor, the DSR 6 reply and the formatted bytes. `perch version --json` calls it and prints the result with the Ghostty commit. This pulls in the terminal, the parser, the formatter and their dependencies, so linking it proves what M1 has to prove.
- The build contract in `build.rs`.
- `NOTICE`: Ghostty (MIT), uucode (MIT, with its Unicode-license data and Björn Höhrmann's MIT decoder), Zig's standard library and `compiler_rt` (MIT), and any other package the audit finds linked.
- `editors/vscode` skeleton: `extensionKind: ["workspace"]`, `engines.vscode` `^1.82.0`, `@types/vscode` `1.82.0`, esbuild targeting Node 18, ESLint, one activation test with `@vscode/test-electron`.
- `ci.yml`, all jobs installing Zig from the pinned tarball and checking its digest:
  - `ubuntu-24.04` and `ubuntu-24.04-arm`: fmt, clippy (`--all-targets -- -D warnings`), `cargo test`, `cargo test --features test-hooks`, then the same two test runs with `--target <arch>-unknown-linux-musl`, which run natively. Then a release build for the musl target, `perch version --json`, `file` (static, no interpreter) and a duplicate-symbol scan for `compiler_rt`. The release binary is uploaded as an artifact.
  - `macos-15`: fmt, clippy, both test runs, a release build with `perch version --json`, and a cross-build of `x86_64-apple-darwin` that is not run.
  - `zig build test` in `perch-vt`; the SPDX script; the Zig package check; `cargo deny` twice.
  - Extension on Node 22 LTS: typecheck, lint, the npm checks, and the tests under `xvfb-run` against VS Code `1.82.0` and `stable`, with the binary downloaded from the Linux job's artifact. On `ubuntu-24.04` the job sets `kernel.apparmor_restrict_unprivileged_userns=0` so Electron's sandbox starts.
  - A `fuzz-build` job on nightly: `cargo fuzz build` and a 20-second run of each target that exists. M1 creates `fuzz/` as an empty cargo-fuzz crate with no targets, so this job and the second `cargo deny` run have something to check from the start.
  - An MSRV job: `cargo check --workspace` with Rust 1.95.
- `advisories.yml` weekly; `fuzz.yml` weekly and on demand (below).
- **Decision recorded in the PR**: whether the static link works on every target. If it does not on some target, the PR implements the design's fallback (a separate `perch-vt` executable copied into the session directory) for that target and says why.

### M2a. `perch-vt`

- Functions: `perch_vt_new(cols, rows, scrollback_lines, scrollback_bytes)`, `perch_vt_free`, `perch_vt_feed(bytes, frame)`, `perch_vt_resize`, `perch_vt_at_ground`, `perch_vt_take_replies` (DSR 6 replies tagged with frames; every other reply dropped by shape, DECXCPR included), `perch_vt_restore(mode, client_cols, client_rows, client_scrollback_rows, history_cap, byte_cap, out)`, `perch_vt_tail(rows, out)` (the last screen and the last 1,000 history lines as text with SGR, no prologue, for `tail.txt`), `perch_vt_title` and `perch_vt_pwd` (for `info`), `perch_vt_mode(n)`, `perch_vt_active_screen`, and `perch_vt_prologue_table`.
- Settings: no kitty image storage (the feature is compiled out); only the `write_pty` effect set in production builds.
- **Test observer.** With `-Dtest-observer=true` (passed by `build.rs` under `test-hooks`), `perch-vt` also records every query the parser dispatches (DSR 5, DSR 6, DA1, DA2, and bracketed-paste set, reset, save and restore) with its kind and the byte offset where it completed, readable through `perch_vt_take_observed`. Production builds do not have it. The tracker's differential tests and the tracker fuzz target use it as their oracle.
- **The mode table.** Every mode Ghostty knows at the pinned commit, each with Ghostty's default, a `prologue_reset` mark and a `replay` mark, with the design's reason for each exclusion. 2026 is reset but not replayed; 2027 is replayed early.
- Zig tests, one or more per capability the restore needs: history with soft wraps joined, styles, trailing blank rows and background-only rows kept, trimmed by rows at a width and by bytes; the primary screen while the alternate is active; state without content through the table; only changed palette entries and OSC 10, 11 and 12; both screens' saved cursors in the design's order, with pending wrap at a blank edge and at a DEC-special character at the edge; the kitty ring rebuilt with zeros, after wraparound; hyperlinks per cell with each region closed; one DSR 6 reply per query frame; `at_ground` after splits inside CSI, OSC, DCS and UTF-8; `tail`; title and pwd. `zig build test` runs these with and without the observer.

### M2b. Boundary tracker

- `tracker.rs`: a port of Ghostty's parser states and UTF-8 handling, reporting ground, the kind of an unfinished sequence, DSR 5, DSR 6, DA1 and DA2 with their end offsets, bracketed paste (set, reset, save, restore, RIS; not DECSTR), and reply shapes in client input. The port is credited in `NOTICE`.
- Tests: unit tests per transition, starting with `C2 9B`, CAN and SUB in CSI and OSC, ESC in OSC, BEL and ST, C0 inside CSI parameters, split UTF-8 at every offset, and `CSI ? 2004 s`, `l`, `r`. A `proptest` differential test feeds random streams in random chunks to the tracker and to `perch-vt` with the test observer, and after every chunk compares ground, the queries' kinds, order and offsets, and bracketed paste.
- Fuzz target `tracker`: the same differential under cargo-fuzz. A disagreement or an abort inside `perch-vt` is a finding. Coverage guidance sees only the Rust side; the Zig side is exercised blind.

### M2c. Restore assembly against real emulators

- `restore.rs`: prologue and epilogue from the mode table, the `full` and `screen` restores around `perch_vt_restore` with 2027 written before content, the hold-back rule, the very-long-sequence rule, and the partial-model restore (notice and resize nudge).
- `perch-testprogs model-dump`: runs an action script (JSON: `feed` chunks, `resize`, `restore {mode, size, scrollback}`, `exit_alt {mode}`) through the tracker and `perch-vt`, and writes each restore's bytes and `perch-vt`'s view of the expected state.
- **Fixtures are committed files.** Each fixture is a byte stream plus an action script under `tests/fidelity/fixtures/`. Program fixtures are recordings made once by `tests/fidelity/record.sh`, which runs R, zsh, fish, `vim`, `less` and a reedline program under `script` on a maintainer machine. Synthetic fixtures (kitty stacks, 2027, resizes, DECSTR, RIS, oversized sequences, split UTF-8) are generated by committed scripts in `tests/fidelity/gen/`. CI only replays the files, so it needs none of those programs. They are also the fuzz seed corpus.
- `tests/fidelity`, Node 22, run with three versions of the headless xterm.js, installed side by side under npm aliases and pinned in the harness lockfile with the Unicode 11 addon release that matches each: `xterm-headless` 5.3.0-beta.61 (what VS Code 1.82, Perch's minimum, ships), `@xterm/headless` 5.5.0, and `@xterm/headless` 6.0.0. Oracles:
  - **xterm.js at the same size**: terminal A gets the original stream, B gets the restore. For an ordinary full restore, the whole buffers, scrollback included, must match cell for cell (characters, colors, attributes, hyperlinks), with the cursor and the modes `IModes` exposes. Then both get the program's own exit from the alternate screen, then more output, and must still match. Where the design limits history (the client's scrollback rows, the 4 MiB cap), B's scrollback must equal the newest part of A's that fits those limits. Where the restore byte cap applies, the check is that B received the prologue, the notice and the resize nudge, and nothing else.
  - **A second `perch-vt` terminal** for what xterm.js does not implement or expose: the kitty stacks, both saved cursors, modes outside `IModes`, palette. Self-referential, and the PR says so.
  - **The model's own state** for history after a width change, because Ghostty and xterm.js reflow differently.
  - **Hostile client state**: every fixture is also restored into a terminal left in the alternate screen, with margins, insert mode, a half-written CSI and an open OSC 8.
  - **`screen` mode** keeps existing scrollback.
- Fixtures, each with named assertions: R prompt output; reedline-style redraws; zsh and fish with right prompts; `less`; `vim` in and out; programs that leave and re-enter the alternate screen with 47, 1047 and 1049 in each order, asserting both saved cursors and the alternate contents after each exit (with xterm.js's clearing of its alternate buffer noted where Ghostty differs); progress bars; wide characters and grapheme clusters; mode 2027 on with width-sensitive text, so a late 2027 would misplace the cursor; hyperlinks; split UTF-8; invalid bytes; RIS; DECSTR; kitty stacks with zero entries, after wraparound, and with further pushes and pops after the restore (checked by the second `perch-vt`); the same kitty fixtures against a test client stack with only 4 entries (a small model in the harness), checking on both screens that it keeps the newest entries in order, zeros included, and that later pushes and pops then behave as the design says; a DEC-special character at the right margin; 200 to 80 to 200 columns; history over the client limit, over 4 MiB, and ending in blank lines; and the restore byte cap's fallback.
- tmux (installed by `apt` and `brew`): for same-size fixtures, `capture-pane -p -e` of panes that ran `cat restore.bin` and `cat original.bin` must match for text and SGR.
- Memory: measure the model at 1,000 and 10,000 lines by 120 and 500 columns, record the numbers in `docs/research/libghostty-build.md`, and assert 120 MB for the largest case.
- Fuzz target `restore`: random streams, then a restore fed to a second `perch-vt` and compared.
- **Continuation evaluation**, last in M2c, using these fixtures. Ghostty's stream continuation replaces the hold-back rule only if: it is exact for every fixture split at every offset; `ContinuationUnavailable` (bounded buffer exhausted) has a tested fallback, including exhaustion in the middle of a screen-affecting CSI and recovery afterwards; and its API is small. If adopted, the PR changes the design (through review), removes the hold-back and 16 MiB rules from `restore.rs`, and the M3 milestones use the continuation. Otherwise the PR records why not.

### M3a. Flow, input and query state machines (pure)

- `flow`: whole-frame queue, 4 MiB live cap, backpressure, stall after 5 seconds with frames pending, drop of unstarted frames only, epochs, restore delivery ahead of live output, `too_slow` after 3 stalls in 10 minutes, PTY reads paused from snapshot request to enqueued restore.
- `input`: FIFO, 1 MiB cap with atomic `queue_full`, replies ahead of unstarted items and never inside a partly written one, the 100 ms hold for a split trailing reply, `paste` from the tracker's state, `submit` as one `\r`.
- `queries`: ownership at snapshot request; DSR 5, DA1 and DA2 answered with fixed replies; DSR 6 from the model's tagged reply within 500 ms; outstanding entries and reply matching; answers on stall drop, detach and takeover; departing client input drained first; 2-second expiry; no DSR 6 from a failed or partial model; the missed-deadline count.
- Tests: scripted scenarios with exact input and output bytes for every transition the design names, a partly written send that a reply must not split, a reply split across reads, expiry, every unanswered case, a model reply after its deadline. Property tests check that no scenario gives two replies to one query.

### M3b. Runtime, metadata, protocol and the detached supervisor

- `sys`, `runtime`, `meta` (frozen fields, opaque start times), `proto` (codec, 1 MiB cap, `Hello`, `Welcome`, `Error`, control ops, 5-second `Hello` deadline).
- Supervisor without a model or attach: double fork, `setsid`, the program forked after readiness for `-d`, umask and directory recorded and restored, signal mask cleared and dispositions reset to default (the supervisor ignores SIGHUP and SIGPIPE, which `exec` would otherwise keep), close-on-exec everywhere, readiness and exec-result pipes, PTY size from the creator's terminal or 80 by 24, clamped to 1 to 500, termios with `IUTF8`, `PERCH_SESSION_ID` and `PERCH_SESSION_DIR`, placeholders, `--env`, `--annotate`, `--kind`, `--owner`, `--cwd`, `--name`, `--id`, `--scrollback` (at most 10,000), the 60-second identity check and republish, the hourly touch of every entry, the capped log, SIGTERM as kill, the kill scope, tombstones (`tail.txt` empty until M3c), 7-day expiry.
- CLI: `new -d`, `list [--json] [--all]`, `info`, `kill [--force] [--pid]`, `dismiss`, `rename`, `annotate`.
- Fuzz target `frame_decode`.
- Tests (real processes, `test-hooks`): golden files for `meta.json`, `list --json` and a `Hello`/`Welcome`/`Error` transcript; exec failure; the program's umask, directory, signal mask and dispositions; open descriptors (`fd-lister` sees only 0, 1, 2); PTY size, clamping and `IUTF8`; environment variables; placeholders; the `--scrollback` ceiling; start-time parsing with `)` and spaces in a process name; kill reaching a shell's background and stopped jobs and a child ignoring SIGHUP; `lost` after SIGKILL of the supervisor, with the log kept; `unresponsive` after SIGSTOP within 500 ms; `kill --pid` refusing a mismatched start time; directory refusals (symlink, mode; owner and peer uid as unit tests); republishing a deleted directory; the hourly touch reaching a foreign file and a foreign socket; 20 parallel `list` during 10 parallel `new -d`; directory `flock` across `rename` on both OSes; protocol errors; survival when the creator's process group and parent are killed; a tombstone on exit while detached.

### M3c. The model process and detached queries

- The model process: forked at start, socket pair and stderr only, frames cut at each query, generations, the 16 MiB backlog limit, the 2-second snapshot deadline, `RLIMIT_AS` on Linux and the RSS check on macOS, the missed-DSR-6 rule, restart at most 3 times per hour at a ground point, `partial` until RIS. `queries` and `input` wired for the detached case, so detached sessions answer DSR 5, DSR 6, DA1 and DA2. `tail.txt` from `perch_vt_tail`, empty with a note when the model is failed or partial. `info` gains `model`, memory over the kill scope, cwd (live, then OSC 7 from the model, then the start directory) and the foreground process.
- Tests: `dsr-timer` detached (replies under 50 ms); the model's descriptors (`fd-lister` equivalent: only its socket and stderr); kill, hang, memory over the limit and three missed DSR 6 deadlines, each through `test-hooks`, each leaving the program running with `info.model` going `failed` then `partial`, a stale-generation reply discarded, and the restart limit held; a model abort's message in `supervisor.log`; `info` fields; `tail.txt` contents.

### M3d. Attach

- `client`: raw mode, non-blocking relay both ways, at most one frame buffered, SIGWINCH to resize frames, the detach key (legacy and CSI-u forms, or `none`), takeover prompt and in-process re-attach, the epilogue, exit with the program's status.
- Supervisor: `Attach`, `Attached`, `Busy`, the program forked after the creator's attach (30-second timeout), takeover, `--if-detached`, restores through the model with `flow`, `queries` for live clients, exit while attached (drain, `exited` frame, directory removed), nesting refusal, `PERCH_SESSION_ID` checked with `getsid`, `detach`.
- CLI: `new` (attaching), `attach`, `detach`.
- End-to-end harness: a Rust test client records what it receives from a real supervisor, and the M2c harness checks the screen.
- Tests: new, attach, detach, reattach; output while detached appears; `full` and `screen` end to end; takeover; `Busy` and exit 75; a creator that never attaches leaves nothing; exit while attached gives the exit code and removes the directory; **no injection** (`stdin-recorder` through attach, detach, takeover, every restore mode, resize and kill receives exactly the typed bytes); the detach key; the epilogue's bytes; **attaching mid-sequence**: a client attaching in the middle of each oversized kind (APC, PM, SOS, DCS, OSC 52, OSC 1337, OSC 5522, and a CSI with line feeds) receives none of the remainder and does receive the ordinary output that follows the sequence's end; the CSI case leaves the model `partial`.

### M3e. Send, live queries and flow end to end, environment, status

- `send` op and CLI. Linked variables. `perch doctor`.
- Tests: `dsr-timer` with at most one reply across attach, takeover, stall and detach at scripted points; `reedline-prompt` (`reedline =0.52.1`, default features off, `crossterm 0.29`) detached with 200 sends and one 900 KB send, no prompt waiting 2 seconds, then the documented stall after a model restart and its end after RIS; reply priority with a write gate, before and during a large send, and with a client reply split across reads; send `queue_full`, no interleaving, `paste` with 2004 on, off, saved and restored, `submit`, `outcome_unknown` on a drop before `queued`; flow with a stopped client (at most 5 seconds), a slow client (no drops), keystrokes and resizes passing under backpressure, stalls mid-restore and mid-live ending with a correct screen, and `too_slow`; linked variables with two `ssh-agent`s and a missing variable; `doctor` on both OSes.

### M4. Linux host checks

`tests/linux/` builds a disposable privileged container from `ubuntu:24.04` with systemd as PID 1, `openssh-server` with PAM, and a test user. It runs locally under colima (arm64) and in CI on both Linux runners. If systemd or logind cannot run in a CI container, the job fails rather than skipping. If that turns out to be a limit of GitHub's runners, the check becomes local-only and the PR says so. Locally, if the container cannot run logind, the scripts use a separate colima profile created for the test and deleted after it. The user's own VM is never changed.

- **SSH drop**: start a session over ssh, kill the client's sshd process, reconnect, attach; the program ran on and the screen restores.
- **logind**: log in over ssh through PAM, check that the supervisor sits in that login's session scope, then end the login normally (exit the ssh session) with `KillUserProcesses=no` and then `yes`. With `no` the session survives a reconnect. With `yes` it is gone, and `perch doctor` had warned.
- **Agent forwarding**: `ssh -A` into the container, create a session, reconnect with a different agent, attach; `ssh-add -l` inside the session reaches the new agent through the linked `SSH_AUTH_SOCK`. This is the remote check the design requires before `SSH_AUTH_SOCK` can be a default.
- **tmpfiles**: an aging rule of 0 seconds; `systemd-tmpfiles --clean` keeps a live session and removes a dead one.
- **Binary deleted and replaced**: start from a binary in a temporary directory, delete it, attach with a binary elsewhere, and kill the model process so it restarts after the binary is gone.
- **Old and minimal userlands**: the musl release binary creates, attaches to and kills a session on `rockylinux:8` and `alpine:3`, on each runner's native architecture.

### M5a. Extension: binary, list, kill

- `src/core/` (no `vscode` imports: recognition, restore sets, retries, list diffs, labels, settings) with plain-Node tests; `src/vscode/` wiring.
- Binary resolution, chmod on activation, `perch.path`, `available()` false on Windows hosts.
- `perch self-install <dest>` (Rust, hidden): `flock` on a lock file, newer-wins under the lock, temporary file and rename, a fallback without `flock`; tested with concurrent processes and with `flock` disabled through `test-hooks`. The extension calls it for `<globalStorage>/bin/<os>-<arch>/`, then `environmentVariableCollection.append('PATH', ...)` and `PERCH_BIN`, under `perch.addToPath`. "Perch: Show Command-Line Path".
- Session list view, refresh cadence, kill confirmation, status bar count and quick pick.
- Labels: `<workspace-key>#<window-key>`, the workspace key being the first 16 hex digits of the SHA-256 of the workspace URI, or `none`; the window key 8 random hex digits per activation.
- Tests: plain-Node core; test-electron for activation, listing a CLI-created session, kill confirmation (mocked modal), `available()` with a mocked platform.

### M5b. Extension: terminals

- Open with confirmation, creation (`prepareLaunch`, `createTerminal` with `isTransient`, readiness, exec errors), the persistent shell profile with `perch.shell`, recognition in `onDidOpenTerminal` and at activation, adoption, restore (`info` first, `--if-detached`, exit 75 confirmed with `info`, 60-second retries), exit reasons, the first-close notice, `--detach-key none` and `--restore full`.
- Settings: `perch.restoreTerminals`, `perch.linkedEnvironment`, `perch.shell`, `perch.path`, `perch.addToPath`, and `terminal.integrated.scrollback` passed to `perch new`.
- Tests: plain-Node recognition, restore sets and retries; test-electron create, close-and-survive, reopen-and-restore, a program really exiting with 75; cross-process restore with two `runTests` launches sharing `--user-data-dir` and a phase marker. Adoption after an extension-host restart: plain-Node tests, and "not run" for the real thing.

### M5c. Extension: API and linked defaults

- API v1 as the design specifies, including `onDidEndSession`, and the one-time `perch doctor` warning. The design's `iconPath` field is typed as `vscode.TerminalOptions['iconPath']`, because `vscode.IconPath` is not in the 1.82 typings.
- Tests: `getApi(1)` and `getApi(2)`; create, send while closed, kill, `onDidEndSession`; `send` error codes; `prepareLaunch` with a `TerminalProfileProvider` and `sessionForTerminal`.
- **Linked-variable defaults, by the design's gate.** `SSH_AUTH_SOCK` is a default only if M4's agent-forwarding check and a local check both pass. `VSCODE_IPC_HOOK_CLI` gets a local test-electron check (the session's variable is the link, the link resolves to the window's socket, `connect()` succeeds), but it stays opt-in in `perch.linkedEnvironment`, because its Remote-SSH check cannot run here. The checklist has that check, and enabling it by default is a later change once it has been run.

### M6. Packaging and release

- Release builds on native runners for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and `aarch64-apple-darwin`, each running `perch new -d`, `list` and `kill`; `x86_64-apple-darwin` cross-compiled and not run. Darwin binaries ad-hoc signed.
- Seven VSIXs (six targets, the musl builds also serving the alpine targets, plus a universal one without a binary), each checked for the binary's presence and architecture.
- Smoke test per native runner: install that runner's own VSIX and the universal one with the `code` CLI of the VS Code that `@vscode/test-electron` downloads (`--extensions-dir` a temporary directory), then run the installed binary. VS Code's CLI does not check a local VSIX's target (1.144 installed a linux-x64 VSIX on darwin-arm64), so installing one proves nothing about another platform. The alpine VSIXs are checked by unpacking them and running their binary in an `alpine:3` container.
- Publishing to the Marketplace and Open VSX: written, gated on a tag and secrets, not run.

### M7. Documentation

README, `docs/usage.md`, `docs/api.md` (integration guide with the Raven and Sight guidance), `docs/cli.md`. Reviewed by Sonnet and Terra until both agree, then merged.

## Verification matrix

| Claim from the design's list | Milestone | How |
|------------------------------|-----------|-----|
| Static link per target, `compiler_rt` | M1 | Real link through `perch_vt_selftest`; fallback if needed |
| `perch-vt` capabilities | M2a | Zig tests per capability |
| Tracker agreement (ground, queries, paste) | M2b | Unit tests; chunked differential with the test observer; fuzz |
| Fidelity contract | M2c, M3d | xterm.js 5.5 and 6.0, second `perch-vt`, model state; tmux text and SGR |
| Mixed 47, 1047, 1049, saved cursors, re-entry | M2c | Fixtures with exits and re-entries |
| Kitty ring with zeros and later pushes and pops | M2c | Second `perch-vt`; a kitty-capable terminal by hand (not run) |
| 2027 before content | M2c | Width-sensitive fixture |
| History seam and blank lines | M2c | Whole-buffer comparison |
| Continuation alternative and `ContinuationUnavailable` | M2c | Evaluation with exhaustion fixtures |
| Oversized sequences and mid-sequence attach | M2c, M3d | Fixtures; end-to-end attach tests |
| Transitions (stalls, takeover mid-restore, split replies) | M3a, M3e | Pure scripted tests; end-to-end |
| Model crash, hang, memory, missed replies, `partial` | M3c, M3e | Fault injection; reedline after restart |
| Reedline detached and behind a large send | M3e | `reedline-prompt` |
| DECSTR, `3J`, `?1049l`, epilogue in other terminals | M2c, M3d | xterm.js and tmux; Terminal.app and iTerm not run |
| Linked variables | M3e, M4, M5c | Two local agents; agent forwarding over ssh; local `code` IPC; Remote-SSH not run |
| Survival: window close, SSH drop, logout, server restart, update | M3b, M4, M5b | Process-group kill; container SSH drop and logind; binary deleted; terminal close |
| tmpfiles, directory `flock` | M3b, M4 | Both OSes; container |
| `creationOptions` on adopted and profile terminals | M5b, M5c | test-electron; restart adoption not run |
| Registry installs, darwin-x64 execution, VSCodium | M6 | Local VSIX smoke tests; the rest not run |
| `PERCH_SESSION_ID` and `getsid` | M3d | Integration test |
| Keystrokes under backpressure | M3e | Integration test |
| Memory after widening | M2c | Measured and bounded |
| Reply behind a large send | M3a, M3e | Pure and write-gated tests |
| Concurrent binary copies | M5a | Rust `self-install` test |
| Shell integration after reattach mid-command | M5b | Not run |
| No injection (brief) | M3d | `stdin-recorder` invariant test |

## Fuzzing

Targets: `tracker` (M2b), `restore` (M2c), `frame_decode` (M3b). On every PR, CI's `fuzz-build` job builds them on nightly and runs each for 20 seconds, so a broken target fails before merge. `fuzz.yml` runs each for 10 minutes weekly and on demand, uploads crashes as artifacts, and uses the M2c fixtures as the seed corpus. An abort in `perch-vt` counts as a crash. Coverage guidance is Rust-only; the Zig code is exercised blind. Fuzzing runs only in CI, because this laptop's Rust has no nightly.

## Risks to the plan

- **Ghostty internals change.** The pin is fixed for v1. An upgrade is its own PR gated by `zig build test`, the differential test and the fidelity suite.
- **The static link may fail on a target.** M1 decides it first, and the design has a fallback.
- **Package hosts.** Zig fetches Ghostty's packages from their hosts. CI's cache keyed by `build.zig.zon` covers outages after the first fetch.
- **VS Code test flakiness.** Tests poll for state with timeouts.
- **Electron in the local sandbox.** test-electron may not run locally; CI is the source of truth for it.
- **logind in CI containers.** If GitHub's runners cannot run it, that check becomes local-only, stated in its PR.

## Review history

### Revision 6

From Sonnet's review of revision 5 (Sol found it ready): `zig build` runs from `$OUT_DIR` with `--build-file` and a relative `ZIG_LOCAL_PKG_DIR`, because an absolute one breaks Ghostty's path dependencies, as Sonnet's test showed; the layout's MSRV says 1.95; test programs are built with the test run's target and features; synthetic fixtures come from committed generator scripts.

### Revision 5

From the review of revision 4 by Sonnet and Sol: Zig's local package directory and outputs go under `$OUT_DIR`, which fixes the rebuild loop and lets the package audit start empty; NCSA for `libfuzzer-sys`; an empty fuzz crate from M1; Ghostty's static-library settings (PIC, bundled `compiler_rt`) and the macOS deployment target on both sides; the test observer wired to `test-hooks`, with a release-binary check that no hook leaked; fixtures are committed recordings; the MSRV is 1.95, which `reedline` needs, with a check job; how tests find the test programs; xterm-headless 5.3.0-beta.61, VS Code 1.82's own, joins the harness, with each version's addon pinned; separate assertions for capped history and the byte-cap fallback; a 4-entry client stack for the smaller-stack case; every discarded sequence kind in the mid-sequence attach tests; per-platform VSIX smoke tests; kitty storage compiled out; the `tracker.rs` license header; the `iconPath` type.

### Revision 4

From the review of revision 3 by Sonnet and Sol: M1's link now exercises Ghostty through Rust; the build contract (Zig discovery, target mapping including gnu targets, flags, caches, rebuilds) is fixed in the plan; tests run with and without `test-hooks` and on musl natively; Zig's digests are committed; the Zig package audit reads a real build; `NOTICE` starts in M1; `cargo deny` covers Perch's own crates and the fuzz workspace; a test observer gives the tracker's differential test a query oracle; the continuation evaluation moved to the end of M2c with an exhaustion fallback as a condition; the pure state machines come before the supervisor, and M3 is split in five; fixtures for kitty zeros and wraparound, 2027, and alternate-screen re-entry; harness on xterm.js 5.5 and 6.0 with whole-buffer comparison; measured memory; tests for descriptors, dispositions, clamps, start-time parsing, the touch of foreign sockets, mid-sequence attach and missed replies; the logind check ends a real login; `SSH_AUTH_SOCK` is verified over ssh agent forwarding and `VSCODE_IPC_HOOK_CLI` stays opt-in; fuzz targets are assigned and built on every PR; a failing required fixture goes back to design review instead of being narrowed.

### Revision 3

Rewritten for the Ghostty design (design revisions 8 to 12).

### Revision 2

From the first review by Sonnet and Sol of the avt-based plan.
