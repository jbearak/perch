# Perch implementation plan

Status: draft for review, revision 3. This plan implements [design.md](design.md). Where they disagree, the design wins and the plan gets fixed.

## Principles

- **Riskiest first.** The Zig library over Ghostty, restore, the query rules and flow control are where the design can be wrong. They are proven first: `perch-vt` with its own tests, then restore against a real terminal emulator, then the query and flow state machines as pure code with a fake clock, all before a supervisor process depends on them.
- **One PR per milestone**, merged before the next starts. Each PR leaves `main` building and tested. Milestones before M5 are not release-quality on their own. Nothing is released before M6.
- **Proof in every PR.** Each milestone lists its tests. CI runs them on Linux and macOS. Anything that cannot be checked here goes in [verification.md](verification.md) with status "not run", and PR bodies say "not run", never "recorded".
- **Small dependency set.** Rust runtime dependencies: `libc`, `serde`, `serde_json`. No async runtime, no argument-parsing crate, no `nix`, no third-party Ghostty bindings. All `libc` calls live in one `sys` module, and all calls into `perch-vt` in one `vt_ffi` module. Zig: Ghostty at the pinned commit and what its `ghostty-vt` module needs with SIMD and kitty graphics off, which is `uucode` (MIT). `translate_c` is used only at build time.
- **No runtime npm dependencies** in the extension. Its bundle contains only Perch's own code. A CI check fails if `dependencies` in `editors/vscode/package.json` is not empty. Development and test packages are pinned by lockfile and listed with their licenses in `docs/licenses.md`, checked in CI against an allow-list.
- **Licenses.** `cargo deny check licenses bans sources` blocks PRs, with an explicit allow-list: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib. Dev-dependencies and the fuzz crate are included. Advisories run on a schedule, not on every PR. A CI step lists the Zig packages in `perch-vt/build.zig.zon`'s resolved graph and fails if one appears that `NOTICE` does not cover. Every source file starts with `// SPDX-License-Identifier: GPL-3.0-only`.
- **Pinned toolchains.** Rust stable with `rust-version` set by the workspace. Zig 0.17.0, downloaded from ziglang.org and checked against its published SHA-256 in CI; locally, the same tarball in a tools directory, never a system install. CI does not use macOS Intel runners: `x86_64-apple-darwin` is cross-compiled on the arm64 runner and not executed.
- **Timers in tests.** A compile-time feature `test-hooks` enables `PERCH_TEST_TIME_SCALE` and fault injection (model crash, model hang, memory pressure). Release builds read neither. Pure state machines take an injected clock.
- **Short socket paths in tests.** Every test sets its own `PERCH_RUNTIME_DIR` under `/tmp`, because macOS's `$TMPDIR` is too long for a Unix socket path.

## Repository layout

```text
Cargo.toml                       workspace; release profile keeps panic = "unwind"
deny.toml, NOTICE
crates/perch/                    library (src/lib.rs) and binary (src/main.rs)
  build.rs                       runs `zig build` for perch-vt and links the static library
  src/sys.rs                     libc: pty, fork, flock, termios, peer creds, utimensat, rlimits, proc info
  src/vt_ffi.rs                  the perch-vt C functions, declared by hand
  src/tracker.rs                 boundary tracker ported from Ghostty's parser (pure)
  src/runtime.rs, src/meta.rs, src/proto.rs
  src/model/                     model process: loop around perch-vt; supervisor-side client
  src/restore.rs                 prologue and epilogue tables, restore assembly
  src/flow.rs, src/input.rs, src/queries.rs   pure state machines
  src/supervisor/, src/client.rs, src/env_links.rs, src/procinfo.rs, src/selfinstall.rs
  src/cli/
  tests/
crates/perch-testprogs/          dsr-timer, stdin-recorder, reedline-prompt, model-dump, exit-with
perch-vt/                        Zig library: build.zig, build.zig.zon, src/, tests
fuzz/                            cargo-fuzz crate, outside the workspace, nightly only
tests/fidelity/                  Node harness (@xterm/headless 6.0.0, @xterm/addon-unicode11)
tests/linux/                     Dockerfiles and scripts for host checks
editors/vscode/                  extension: src/core/ (plain logic), src/vscode/ (adapter)
docs/
.github/workflows/               ci.yml, fuzz.yml, advisories.yml, release.yml
```

## Milestones

### M0. Design and plan

Branch `docs/design`: `docs/design.md`, `docs/plan.md`, `docs/research/`, `docs/verification.md`, README links. The design's review history records its reviews by Sonnet and Terra; this plan's review history records its reviews by Sonnet and Sol.

### M1. Skeleton, toolchains and the static link

- Workspace with `crates/perch` (library plus binary, `perch version [--json]`) and `crates/perch-testprogs`.
- `perch-vt/` with a minimal C function (`perch_vt_version`) and Ghostty pinned in `build.zig.zon` at `521cf2c` with `-Dsimd=false` and kitty graphics off.
- `crates/perch/build.rs` maps the Cargo target to a Zig target (`x86_64-linux-musl`, `aarch64-linux-musl`, `aarch64-macos`, `x86_64-macos`), runs `zig build -Doptimize=ReleaseSafe`, and links `libperch_vt.a`. It sets the macOS deployment target to 11.0.
- **Link spike, decided in this PR.** Build and run `perch version` for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and `aarch64-apple-darwin`; build `x86_64-apple-darwin` without running it. Check for duplicate `compiler_rt` symbols and for any dynamic dependency in the musl binaries (`file`, `ldd`). If the static link cannot be made to work on a target, implement the design's fallback (a separate `perch-vt` executable copied into the session directory) and record why.
- `editors/vscode` skeleton: `package.json` with `extensionKind: ["workspace"]`, `engines.vscode` `^1.82.0`, `@types/vscode` pinned to `1.82.0`, esbuild targeting Node 18, ESLint, one activation test with `@vscode/test-electron`.
- `ci.yml`:
  - Rust on `ubuntu-24.04` and `macos-15`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check licenses bans sources`, `zig build test` in `perch-vt`.
  - musl jobs: `x86_64-unknown-linux-musl` on `ubuntu-24.04` and `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`, each running `perch version`.
  - darwin cross job: `x86_64-apple-darwin` built on `macos-15`.
  - Zig package and `NOTICE` check.
  - Extension on Node 22 LTS: typecheck, lint, the npm checks, and the tests under `xvfb-run` against VS Code `1.82.0` and `stable`.
- `advisories.yml`: weekly `cargo deny check advisories`.

Proof: CI green on every job; the link spike's result in the PR body.

### M2a. `perch-vt` and the continuation spike

The Zig library's functions, each proven by Zig tests against Ghostty itself.

- `perch_vt_new(cols, rows, scrollback_lines, scrollback_bytes)`, `perch_vt_free`, `perch_vt_feed(bytes, frame)`, `perch_vt_resize`, `perch_vt_at_ground`, `perch_vt_take_replies` (DSR 6 replies tagged with frame numbers; every other reply dropped by shape, DECXCPR included), `perch_vt_restore(mode, client_cols, client_rows, client_scrollback_rows, history_cap, byte_cap, out)`, plus accessors used by tests (`active_screen`, `mode(n)`, cursor).
- Settings: kitty image storage limit zero, only the `write_pty` effect set.
- Zig tests, one or more for each capability the design's restore section uses:
  - history formatted with soft-wrapped rows joined, styles kept, trailing blank rows and background-only rows kept, trimmed to a row count at a given width and to a byte cap;
  - the primary screen formatted while the alternate screen is active;
  - terminal state with no content, filtered through the replay allowlist;
  - only changed palette entries and OSC 10, 11 and 12 colors;
  - both screens' saved cursors, written by the step-5 order without `ESC 7` and `ESC 8` for pending wrap, including a blank edge cell and a DEC-special character set at the edge;
  - the kitty stack rebuilt from the 8-entry ring on each screen;
  - hyperlinks per cell, with each region closing an open link;
  - one DSR 6 reply per query frame, with several queries in one feed cut into frames;
  - `at_ground` after splits inside CSI, OSC, DCS and UTF-8.
- **The replay table.** A Zig table of every mode Ghostty knows at the pinned commit, each marked replay or exclude with the design's reason, and Ghostty's default. The prologue resets are generated from the same table and exported to Rust.
- **Continuation spike, decided in this PR.** Try Ghostty's stream continuation (`writeContinuation`, `continuation_max_bytes`, `BoundaryScanner`) as the design's alternative to holding back unfinished sequences. Adopt it only if it is exact for every case in the M2c fixtures and its API is small enough to keep up with. The PR records the decision and updates the design if it is adopted.

Proof: `zig build test` green in CI on both OSes.

### M2b. Boundary tracker

- `tracker.rs`: a port of Ghostty's parser states and UTF-8 handling, reporting ground, sequence kind (for the very-long-sequence rule), DSR 5, DSR 6, DA1 and DA2 queries with their end offsets, bracketed paste (set, reset, save, restore, RIS; not DECSTR), and reply shapes in client input.
- Tests:
  - Unit tests per transition, starting from known cases: `C2 9B` (ignored by Ghostty), CAN and SUB inside CSI and OSC, ESC inside OSC, BEL and ST terminators, C0 inside CSI parameters, split UTF-8 at every offset, `CSI ? 2004 s`, `l`, `r`.
  - Differential test with `proptest` (stable Rust): random byte streams, cut into random chunks, fed to the tracker and to `perch-vt` through `vt_ffi`. After every chunk they must agree on ground, on the kinds and order of queries, and on bracketed paste.

### M2c. Restore assembly, proven against xterm.js

- `restore.rs`: the prologue and epilogue (generated from the replay table), the `full` and `screen` restores around `perch_vt_restore`, the hold-back rule (or the continuation, per M2a), the very-long-sequence rule, and the partial-model restore (notice and resize nudge).
- `perch-testprogs model-dump`: runs an action script (JSON list of `feed` chunks, `resize`, `restore {mode, size, scrollback}`) through the tracker and `perch-vt` and writes each restore's bytes plus `perch-vt`'s view of the expected state.
- `tests/fidelity` harness (Node 22, `@xterm/headless` 6.0.0 and `@xterm/addon-unicode11` with Unicode 11 active, as VS Code uses). Three oracles, each used for what it can see:
  - **xterm.js, same size**: terminal A receives the original stream, terminal B receives the restore. B must equal A in every visible cell (characters, colors, attributes, hyperlinks), the cursor, and the modes `IModes` exposes. Then both receive `ESC [?1049l` or the program's own exit sequence, and their primary screens must match.
  - **A second `perch-vt` terminal**: the restore bytes are fed to a fresh Ghostty terminal, whose state is compared with the source model's for what xterm.js does not implement or expose (kitty stacks, both saved cursors, modes outside `IModes`, palette). This is self-referential, and the PR says so.
  - **The model's own state** for history after a width change, because Ghostty and xterm.js reflow differently.
  - **Hostile client state**: each fixture is also restored into a terminal left in the alternate screen, with margins, insert mode, a half-written CSI and an open OSC 8.
  - **`screen` mode**: a terminal with existing scrollback keeps it.
- Fixtures: R prompt output, reedline-style prompt redraws, zsh and fish with right prompts, `less`, `vim` in and out, programs mixing 47, 1047 and 1049, `\r` progress bars, wide characters and grapheme clusters, hyperlinks, split UTF-8, invalid bytes, RIS, DECSTR, kitty flags on both screens, a DEC-special character set at the right margin, resizes from 200 to 80 to 200 columns, history over the client limit and over 4 MiB, history ending in blank lines, and sequences over 16 MiB (OSC 52, APC, DCS, and a CSI with line feeds in its parameters).
- tmux as a second real terminal (installed with `apt` and `brew` in CI): for same-size fixtures, `tmux capture-pane -p -e` of a pane that ran `cat restore.bin` must equal that of a pane that ran `cat original.bin`, for text and SGR.
- Memory: 10,000 lines widened to 500 columns stays under the design's model limit.

Proof: harness green in CI. A fixture that cannot pass is fixed, or recorded in the design's "Not covered" list in the same PR.

### M3a. Runtime, metadata, protocol, detached supervisor and the model process

- `sys`, `runtime` (directory checks, publish, lock-then-read `list`, `lost`, removal under exclusive lock with inode revalidation, 10-second minimum age), `meta` (frozen fields, opaque start times), `proto` (codec, 1 MiB cap, `Hello`, `Welcome`, `Error`, control ops, 5-second `Hello` deadline).
- Supervisor without attach: double fork, `setsid`, umask and directory recorded and restored in the child, signal mask cleared, close-on-exec everywhere, readiness and exec-result pipes, the PTY sized from the creator's terminal and termios (`IUTF8` included) or 80 by 24 with `-d`, placeholders, `--env`, `--annotate`, `--kind`, `--owner`, `--cwd`, `--name`, `--id`, the 60-second identity check and republish, the hourly touch, the capped log, SIGTERM as kill, the kill scope, tombstones, 7-day expiry.
- The model process: forked at start, socket pair only, frames cut at each query, generations, the 16 MiB backlog limit, 2-second snapshot deadline, `RLIMIT_AS` on Linux and the RSS check on macOS, the missed-DSR-6 rule, restart at most 3 times per hour at a ground point, `partial` until RIS.
- CLI: `new -d`, `list [--json] [--all]`, `info`, `kill [--force] [--pid]`, `dismiss`, `rename`, `annotate`.
- `fuzz/` target for the frame decoder.
- Tests (real processes, `test-hooks` timer scaling):
  - Golden files for `meta.json`, `list --json` and a `Hello`/`Welcome`/`Error` transcript.
  - Exec failure; the program's umask, directory and signal mask; PTY size and `IUTF8`; placeholder expansion and escapes.
  - Kill reaches a shell's background and stopped jobs; a child ignoring SIGHUP gets SIGKILL.
  - SIGKILL of the supervisor gives `lost` with the log kept; SIGSTOP gives `unresponsive` within 500 ms.
  - `kill --pid` refuses a PID with a different start time.
  - Runtime directory refusals (symlink, wrong mode; foreign owner and peer uid as unit tests with injected values).
  - A deleted live session directory is republished and marked `degraded`.
  - 20 parallel `list` during 10 parallel `new -d` remove nothing live or starting.
  - Directory `flock` survives `rename` on both OSes.
  - Protocol: version mismatch, unknown op, missing `Hello`, oversize frame.
  - Survival: the creator's process group and parent killed with SIGHUP and SIGKILL; the session lives.
  - Model process: killed, hung and over its memory limit (all through `test-hooks`); each time the program keeps running, `info.model` shows `failed` then `partial`, a stale-generation reply is discarded, and the restart limit holds.
  - Exit while detached leaves a tombstone with the right status and `tail.txt`.

### M3b. Flow, input and query state machines (pure)

- `flow`: whole-frame queue, 4 MiB live cap, backpressure, stall after 5 seconds with frames pending, drop of unstarted frames only, epochs, restore delivery ahead of live output, `too_slow` after 3 stalls in 10 minutes, PTY reads paused from snapshot request to enqueued restore.
- `input`: FIFO, 1 MiB cap with atomic `queue_full`, replies ahead of unstarted items and never inside a partly written one, the 100 ms hold for a split trailing reply, `paste` from the tracker's bracketed-paste state, `submit` as one `\r`.
- `queries`: ownership at snapshot request; DSR 5, DA1 and DA2 answered by fixed replies; DSR 6 answered from the model's tagged reply within 500 ms; outstanding entries and reply matching; answers on stall drop, detach and takeover; departing client input drained first; 2-second expiry; no DSR 6 from a failed or partial model.
- Tests: scripted scenarios with exact expected input and output bytes for every transition the design names, including a partly written send that a reply must not split, a reply split across two reads, expiry, each unanswered case, and a model reply arriving after its deadline. Property tests check that no scenario produces two replies to one query.

### M3c. Attach

- `client`: raw mode, non-blocking relay both ways, at most one frame buffered, SIGWINCH to resize frames, detach key (legacy and CSI-u forms, or `none`), takeover prompt and in-process re-attach, the epilogue on detach and exit, exit with the program's status.
- Supervisor: `Attach`, `Attached`, `Busy`, the program forked after the creator's attach (30-second timeout), takeover, `--if-detached`, restores through the model, exit while attached, nesting refusal, `PERCH_SESSION_ID` checked with `getsid`, `detach`.
- CLI: `new` (attaching), `attach`, `detach`.
- End-to-end harness: a Rust test client speaks the protocol to a real supervisor and records what it receives; the M2c harness checks the screen from that recording.
- Tests: new, attach, detach, reattach; output while detached appears; `full` and `screen` end to end; takeover; `Busy` and exit 75; a creator that never attaches leaves nothing; **no injection** (`stdin-recorder` through attach, detach, takeover, every restore mode, resize and kill receives exactly the typed bytes); the detach key; the epilogue's bytes.

### M3d. Send, queries and flow end to end, environment, status

- Wire `input`, `queries` and `flow` into the supervisor. `send` op and CLI. Linked variables. `info` live fields, memory, cwd, foreground. `perch doctor`.
- Tests:
  - `dsr-timer`: detached replies under 50 ms; at most one reply across attach, takeover, stall and detach at scripted points; the unanswered cases as the design says.
  - `reedline-prompt` (`reedline =0.52.1`, default features off, `crossterm 0.29`, as Raven pins them) runs `Reedline::read_line`. Detached, fed 200 lines by `send` plus one 900 KB send: no prompt waits 2 seconds. After a model restart: the stall the design documents, and its end after RIS.
  - Reply priority with a write gate, for a reply arriving before and during a large send, and for a client reply split across two reads.
  - Send: `queue_full`, no interleaving, `paste` with 2004 on, off, saved and restored, `submit`, `outcome_unknown` on a connection dropped before `queued`.
  - Flow: a stopped client holds the program at most 5 seconds; a slow client slows it without drops; keystrokes and resizes pass while the client's output is blocked; stalls mid-restore and mid-live end with a correct screen; `too_slow`.
  - Linked variables with two `ssh-agent`s; a missing variable leaves a dangling link.
  - `doctor` output on both OSes.

### M4. Linux host checks

`tests/linux/` builds a disposable privileged container from `ubuntu:24.04` with systemd as PID 1, `openssh-server` and a test user. It runs locally under colima (arm64) and in CI on both Linux runners. If logind does not work inside the container, the scripts fall back to a separate colima profile created for the test and deleted after it. The user's own VM is never changed.

- SSH drop: start a session over ssh, kill the client's sshd process, reconnect, attach; the program ran on and the screen restores.
- logind: `KillUserProcesses=no` keeps the session after `loginctl terminate-session`; `yes` ends it, and `perch doctor` warned.
- tmpfiles: an aging rule of 0 seconds; `systemd-tmpfiles --clean` keeps a live session and removes a dead one.
- Binary deleted and replaced: start from a binary in a temporary directory, delete it, attach with a binary elsewhere. The model process restarts correctly after the binary is gone (fork without exec).
- Old and minimal userlands: the static musl binary creates, attaches to and kills a session on `rockylinux:8` and `alpine:3`, on each runner's native architecture.

### M5a. Extension: binary, list, kill

- Thin adapter: `src/core/` without `vscode` imports (recognition, restore sets, retries, list diffs, labels, settings), unit-tested in plain Node; `src/vscode/` wires it.
- Binary resolution, chmod on activation, `perch.path`, `available()` false on Windows hosts.
- `perch self-install <dest>` (Rust, hidden): `flock` on a lock file, newer-wins under the lock, temporary file and rename, a fallback without `flock`; tested with concurrent processes and with `flock` disabled via `test-hooks`. The extension calls it for `<globalStorage>/bin/<os>-<arch>/`, then `environmentVariableCollection.append('PATH', ...)` and `PERCH_BIN`, under `perch.addToPath`. "Perch: Show Command-Line Path".
- Session list view, refresh cadence, kill confirmation, status bar count and quick pick.
- Labels: `<workspace-key>#<window-key>`, the workspace key being the first 16 hex digits of the SHA-256 of the workspace URI, or `none`; the window key 8 random hex digits per activation.
- Tests: plain-Node core tests; test-electron for activation, listing a CLI-created session, kill confirmation (mocked modal), `available()` with a mocked platform.

### M5b. Extension: terminals

- Open with confirmation, creation (`prepareLaunch`, `createTerminal` with `isTransient`, readiness wait, exec errors), the persistent shell profile with `perch.shell`, recognition in `onDidOpenTerminal` and at activation, adoption, restore (`info` first, `--if-detached`, exit 75 confirmed with `info`, 60-second retries), exit reasons, the first-close notice, `--detach-key none` and `--restore full`.
- Settings: `perch.restoreTerminals`, `perch.linkedEnvironment`, `perch.shell`, `perch.path`, `perch.addToPath`, and `terminal.integrated.scrollback` passed to `perch new`.
- Tests: plain-Node recognition, restore sets and retries; test-electron create, close-and-survive, reopen-and-restore, and a program really exiting with 75; cross-process restore with two `runTests` launches sharing `--user-data-dir` and a phase marker. Adoption after an extension-host restart is covered by plain-Node tests and listed as not run.

### M5c. Extension: API and environment checks

- API v1 as the design specifies, including `onDidEndSession`, and the one-time `perch doctor` warning.
- Tests: `getApi(1)` and `getApi(2)`; create, send while closed, kill, `onDidEndSession`; `send` error codes; `prepareLaunch` with a `TerminalProfileProvider` and `sessionForTerminal`.
- Linked `VSCODE_IPC_HOOK_CLI` local check in test-electron. `SSH_AUTH_SOCK` ships as a default; `VSCODE_IPC_HOOK_CLI` ships as a default only if this check passes, and the docs say it is not verified over Remote-SSH.

### M6. Packaging and release

- Builds on native runners: `x86_64-unknown-linux-musl` (`ubuntu-24.04`), `aarch64-unknown-linux-musl` (`ubuntu-24.04-arm`), `aarch64-apple-darwin` (`macos-15`), each running `perch new -d`, `list` and `kill`. `x86_64-apple-darwin` is cross-compiled on `macos-15` and not run. Darwin binaries are ad-hoc signed.
- Seven VSIXs (six targets, the musl builds serving the alpine targets too, plus a universal one without a binary), each checked for the binary's presence and architecture.
- Smoke test per native runner: `code --install-extension <vsix> --extensions-dir <tmp>` and run the installed binary.
- Publishing to the Marketplace and Open VSX is written, gated on a tag and on secrets, and not run. `verification.md` lists the registry checks and the darwin-x64 execution as not run.

### M7. Documentation

README, `docs/usage.md`, `docs/api.md` (integration guide with the Raven and Sight guidance), `docs/cli.md`, `NOTICE`. Reviewed by Sonnet and Terra until both agree, then merged.

## Verification matrix

| Claim from the design's list | Milestone | How |
|------------------------------|-----------|-----|
| `perch-vt` capabilities | M2a | Zig tests per capability |
| Static link per target, `compiler_rt` | M1 | Link spike in CI; fallback if needed |
| Continuation alternative | M2a | Spike with decision in the PR |
| Tracker agreement (ground, queries, paste) | M2b | Unit tests and chunked differential test |
| Fidelity contract | M2c, M3c | xterm.js, second `perch-vt`, model state; tmux text and SGR |
| Mixed 47, 1047, 1049 and saved cursors | M2c | Fixtures with exit sequences |
| History seam and blank lines | M2c | Fixtures |
| Oversized sequences | M2c, M3d | Fixtures; end-to-end attach mid-sequence |
| Transitions (stalls, takeover mid-restore, split replies) | M3b, M3d | Pure scripted tests; end-to-end harness |
| Model crash, hang, memory, `partial` | M3a, M3d | Fault injection; reedline after restart |
| Reedline detached and behind a large send | M3d | `reedline-prompt` |
| DECSTR, `3J`, `?1049l`, epilogue in other terminals | M2c, M3c | xterm.js and tmux automated. Terminal.app and iTerm: not run |
| Linked variables | M3d, M5c | Two agents; local `code` IPC. Remote-SSH: not run |
| Survival: window close, SSH drop, server restart, update | M3a, M4, M5b | Process-group kill; container SSH drop; binary deleted; terminal close |
| logind, tmpfiles, directory `flock` | M3a, M4 | Both OSes; container |
| `creationOptions` on adopted and profile terminals | M5b, M5c | test-electron; restart adoption not run |
| Registry installation, darwin-x64 execution | M6 | Local VSIX smoke tests. Registry and darwin-x64: not run |
| `PERCH_SESSION_ID` and `getsid` | M3c | Integration test |
| Keystrokes under backpressure | M3d | Integration test |
| Memory after widening | M2c | RSS bound test |
| Reply behind a large send | M3b, M3d | Pure and write-gated tests |
| Concurrent binary copies | M5a | Rust `self-install` test |
| Shell integration after reattach mid-command | M5b | Not automatable here; not run |
| No injection (brief) | M3c | `stdin-recorder` invariant test |

## Fuzzing

`fuzz.yml` runs weekly and on demand. It installs a pinned nightly with `rustup`, `cargo-fuzz`, and Zig 0.17.0, and runs each target for 10 minutes: `tracker` (differential against `perch-vt`, so a disagreement or an abort in `perch-vt` is a finding), `restore` (random streams, then a restore checked by feeding it to a second `perch-vt`), and `frame_decode`. Crashes are uploaded as artifacts. Fuzzing is CI-only, because this laptop's Rust toolchain is Homebrew's, without nightly.

## Risks to the plan

- **Ghostty internals change.** The pin is fixed for v1. An upgrade is its own PR gated by `zig build test`, the differential test and the fidelity suite.
- **The static link may not work on every target.** M1 decides this first, and the design has a fallback.
- **VS Code test flakiness.** Tests poll for state with timeouts.
- **Electron in the local sandbox.** test-electron may not run locally; CI is the source of truth for it.
- **logind in a container.** If neither the container nor a dedicated colima profile can run it, that check is recorded as not run.

## Review history

### Revision 3

Rewritten for design revisions 8 to 12 (Ghostty's terminal library through `perch-vt`): new M1 link spike and Zig toolchain, a `perch-vt` milestone with capability tests and the continuation spike, the tracker's differential test against `perch-vt`, a second Ghostty terminal as an oracle for what xterm.js cannot show, model-process fault injection, no macOS Intel runner. Revision 2 was reviewed by Sonnet and Sol, and those reviews were superseded by the change of emulator before they finished.

### Revision 2

From the first review by Sonnet and Sol: split milestones into reviewable PRs; pure state machines before the supervisor; action-based fixtures and per-feature oracles; an end-to-end harness; tests for every design item; `perch self-install`; a restart-aware restore test; fuzzing as a nightly job; a concrete build matrix; Linux host checks in a container; honest "not run" items; npm license policy; VS Code 1.82 tested.
