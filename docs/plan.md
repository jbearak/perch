# Perch implementation plan

Status: draft for review, revision 2. This plan implements [design.md](design.md). Where they disagree, the design wins and the plan gets fixed.

## Principles

- **Riskiest first.** Restore, the query rules and flow control are where the design can be wrong. They are built and proven as pure code against a real terminal emulator, with a fake clock, before a supervisor process depends on them.
- **One PR per milestone**, merged before the next starts. Each PR leaves `main` building and tested. Milestones before M5 are not release-quality on their own. Nothing is released before M6.
- **Proof in every PR.** Each milestone lists its tests. CI runs them on Linux and macOS. Anything that cannot be checked here goes in [verification.md](verification.md) with status "not run", and PR bodies say "not run", never "recorded".
- **Small dependency set.** Runtime Rust dependencies: `avt` (`=0.19.0`), `libc`, `serde`, `serde_json`. No async runtime, no argument-parsing crate, no `nix`, no `vte`. All `libc` calls live in one `sys` module.
- **No runtime npm dependencies** in the extension. Its bundle contains only Perch's own code. A CI check fails if `dependencies` in `editors/vscode/package.json` is not empty. Development and test packages are pinned by lockfile and listed with their licenses in `docs/licenses.md`, checked in CI against an allow-list.
- **Licenses.** `cargo deny check licenses bans sources` blocks PRs. The allow-list is explicit: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib. Dev-dependencies and the fuzz crate are included. `cargo deny check advisories` runs on a schedule and on demand, not on every PR. Every source file starts with `// SPDX-License-Identifier: GPL-3.0-only`.
- **Timers in tests.** A compile-time feature `test-hooks` enables `PERCH_TEST_TIME_SCALE` and fault injection. Release builds do not read either, so no environment variable can shorten a deletion timer in a real session. Pure state machines take an injected clock.
- **Short socket paths in tests.** Every test sets its own `PERCH_RUNTIME_DIR` under `/tmp`, because macOS's `$TMPDIR` is too long for a Unix socket path.

## Repository layout

```text
Cargo.toml                       workspace; rust-version = "1.86" (avt's MSRV); edition 2024
                                 release profile keeps panic = "unwind"
deny.toml
crates/perch/                    library (src/lib.rs) and binary (src/main.rs)
  src/sys.rs                     libc: pty, fork, flock, termios, peer creds, utimensat, proc info
  src/runtime.rs                 runtime dir checks, publish, list, cleanup
  src/meta.rs                    meta.json schema and atomic writes
  src/proto.rs                   frame codec and messages
  src/vt/                        utf8 decoder, DEC parser, mode state, query and reply detection
  src/model/                     avt wrapper, restore builder
  src/flow.rs                    client output queue and flow control (pure, fake clock)
  src/input.rs                   input queue with reply priority (pure)
  src/queries.rs                 query ownership (pure)
  src/supervisor/                event loop wiring the pure parts to sockets and the PTY
  src/client.rs                  attach client
  src/env_links.rs, src/procinfo.rs, src/selfinstall.rs
  src/cli/                       one file per subcommand
  tests/                         integration tests with real PTYs
crates/perch-testprogs/          test programs, built only for tests (required-features):
                                 dsr-timer, stdin-recorder, reedline-prompt, model-dump, exit-with
fuzz/                            cargo-fuzz crate, outside the workspace, nightly only
tests/fidelity/                  Node harness (@xterm/headless 6.0.0, @xterm/addon-unicode11)
tests/linux/                     Dockerfiles and scripts for host checks
editors/vscode/                  extension: src/core/ (plain logic), src/vscode/ (adapter)
docs/
.github/workflows/               ci.yml, fuzz.yml, advisories.yml, release.yml
```

## Milestones

### M0. Design and plan

Branch `docs/design`: `docs/design.md`, `docs/plan.md`, `docs/research/emulator-spike.md`, `docs/verification.md` (the maintainer checklist, every item "not run"), README links. The design's review history records the Sonnet and Terra design reviews. The plan records its Sonnet and Sol reviews in its own review history.

### M1. Skeleton and CI

- Workspace, `crates/perch` as library plus binary with `perch version [--json]`, `crates/perch-testprogs` empty.
- `editors/vscode` skeleton: `package.json` with `extensionKind: ["workspace"]`, `engines.vscode` `^1.82.0`, `@types/vscode` pinned to `1.82.0`, esbuild targeting Node 18 (the Node in VS Code 1.82), ESLint, one activation test with `@vscode/test-electron`.
- `ci.yml`:
  - Rust on `ubuntu-24.04` and `macos-15`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check licenses bans sources`.
  - Build-only musl jobs: `x86_64-unknown-linux-musl` on `ubuntu-24.04`, `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`, both with `musl-tools` and `rustup target add`.
  - Extension on Node 22 LTS: typecheck, lint, the npm checks, and the tests under `xvfb-run` against VS Code `1.82.0` and `stable`.
- `advisories.yml`: weekly `cargo deny check advisories`.

Proof: CI green.

### M2a. Text and escape parsing

- `vt::Utf8Decoder`: incremental, at most 3 pending bytes, U+FFFD per maximal invalid sequence, reports the byte offset of each character.
- `vt::Parser`: the DEC state machine (Paul Williams' diagram, as avt implements it) over decoded characters, C1 controls included. Emits CSI, ESC, OSC, DCS, APC/PM/SOS boundaries with their byte offsets, and reports whether it is at ground.
- `vt::Modes`: every mode in the design's "Input modes" list, per-screen kitty stacks (16 entries), title, OSC 7, control-string state, RIS and DECSTR resets. Query detection (DSR 5, DSR 6, DA1, DA2) and reply-shape detection for client input.
- Tests:
  - Unit tests per sequence and per mode.
  - Known counterexamples first: `C2 9B` (UTF-8 CSI), CAN and SUB inside CSI and OSC, ESC inside OSC, BEL and ST terminators, split UTF-8 at every offset.
  - Differential test, stable Rust with `proptest` (dev-dependency): random byte streams fed to `avt::Parser` and `vt::Parser` must agree on ground state after every character (avt's `Parser::dump()` is empty exactly at ground).

### M2b. Model and restore

- `model::Model`: avt with the scrollback set at creation (default 1,000, ceiling 10,000), size clamp 1 to 500. Every call into avt and `vt` runs under `catch_unwind`. A failure marks the model failed and is reported. `feed` splits each read at query offsets and returns events with the cursor at each query. Tracks ground state of both the parser and the decoder.
- `model::restore`: the prologue, `full` and `screen` restores in the design's order, history trimmed by client rows and the 4 MiB cap, the ground-state rule, and the fallback (1 second idle or 5 seconds total, injected clock) that feeds avt `CAN` and withholds the rest of the sequence.
- `perch-testprogs model-dump`: runs an **action script** (JSON list of `feed` chunks, `resize`, `clock` advances, `restore {mode, size, scrollback}`) and writes each restore's bytes plus the model's expected state (cells, cursor, modes, history lines) as JSON.
- `tests/fidelity` harness (Node 22, `@xterm/headless` 6.0.0 and `@xterm/addon-unicode11` with Unicode 11 active, as VS Code uses):
  - **Same-size restores**: terminal A receives the original stream; terminal B receives the restore. B must equal A in every visible cell (character, colors, attributes), cursor, and the xterm.js modes in `IModes`: application cursor and keypad, bracketed paste, insert, origin, reverse wraparound, send focus, mouse tracking mode, and cursor visibility.
  - **Different-size restores and history**: the oracle is the model's own state from `model-dump`, because xterm.js and avt reflow differently, and avt's ED2 pushes the screen into history where xterm.js does not.
  - **Modes xterm.js cannot show** (kitty stacks, modifyOtherKeys, mouse encodings, alternate scroll, 2026): the restore bytes are replayed into a fresh `vt::Modes` and compared with the source. This test is self-referential, and the PR says so. kitty fixtures also check that the bytes contain the pops and pushes in the design's order.
  - **Alternate screen**: after restoring into B while the program is in `vim`, both A and B receive `ESC [?1049l`, and their primary screens must match.
  - **Hostile client state**: each fixture is also restored into a terminal left in the alternate screen, with margins, insert mode, a half-written CSI and an open OSC 8. The result must still match.
  - **`screen` mode**: a terminal with existing scrollback keeps it.
- Fixtures, as action scripts: R prompt output, reedline-style prompt redraws, zsh and fish with right prompts, `less`, `vim` in and out, `\r` progress bars, wide characters, split UTF-8, invalid bytes, RIS, DECSTR, kitty push and pop on both screens, OSC 8 left open, resizes 200 to 80 to 200, history over the client limit and over 4 MiB, a sequence unfinished for 1 second, one unfinished for 5 seconds with output continuing, and the 1-second fallback cutting a DSR 6.
- tmux as a second terminal (CI installs tmux with `apt` and `brew`): for each same-size fixture, a detached tmux pane at the fixture's size runs `cat restore.bin` and then sleeps; `tmux capture-pane -p -e` must equal the same capture of a pane that ran `cat original.bin`. This compares text and SGR only, which is what tmux exposes. Ubuntu's and Homebrew's tmux differ in version, so the comparison is between two panes of the same tmux.
- Memory: a test feeds 10,000 lines, widens to 500 columns and asserts RSS below 120 MB.

Proof: harness green in CI. A fixture that cannot pass is fixed, or recorded in the design's "Not covered" list in the same PR.

### M3a. Runtime, metadata, protocol, detached supervisor

- `sys`, `runtime` (checks, publish, lock-then-read `list`, `lost`, removal under exclusive lock with inode revalidation, 10-second minimum age), `meta` (frozen fields, opaque start times), `proto` (codec, 1 MiB cap, `Hello`, `Welcome` with capabilities, `Error`, control ops, 5-second `Hello` deadline).
- Supervisor without attach: double fork, `setsid`, umask and directory recorded and restored in the child, signal mask cleared, close-on-exec everywhere, readiness pipe, exec-result pipe, the PTY sized from the creator's terminal and termios (`IUTF8` included) or 80 by 24 with `-d`, placeholders `{dir}`, `{id}`, `{{`, `}}`, `--env`, `--annotate`, `--kind`, `--owner`, `--cwd`, `--name`, `--id`, 60-second identity check and republish, hourly touch of every entry with `AT_SYMLINK_NOFOLLOW`, `supervisor.log` capped at 256 KiB, SIGTERM as kill, kill scope (session scan, SIGHUP then SIGCONT, SIGKILL after 5 seconds), tombstones with `tail.txt`, 7-day expiry.
- CLI: `new -d`, `list [--json] [--all]`, `info`, `kill [--force] [--pid]`, `dismiss`, `rename`, `annotate`.
- `fuzz/` target for the frame decoder.
- Tests (real processes, `test-hooks` timer scaling):
  - Golden files for `meta.json`, `list --json` and a `Hello`/`Welcome`/`Error` transcript, so the frozen set cannot drift.
  - Exec failure; program umask, directory and signal mask; PTY size and `IUTF8`; placeholder expansion and escapes.
  - Kill reaches a shell's background and stopped jobs; a child ignoring SIGHUP gets SIGKILL.
  - SIGKILL of the supervisor gives `lost` with the log kept; SIGSTOP gives `unresponsive` within 500 ms.
  - `kill --pid` refuses a PID with a different start time.
  - Runtime directory refusals (symlink, foreign owner as a unit test with injected `stat`, wrong mode). Peer-uid rejection as a unit test with injected credentials, since CI has one user.
  - Deleting a live session directory leads to republishing and `degraded`.
  - 20 parallel `list` during 10 parallel `new -d` never remove a live or starting session.
  - Directory `flock` survives `rename` (both OSes).
  - Protocol: unknown version gives `Error` with both ranges; unknown op gives `unsupported`; no `Hello` in 5 seconds closes; an oversize frame closes.
  - Survival: the creator's process group and its parent are killed with SIGHUP and SIGKILL; the session lives.
  - Exit while detached leaves a tombstone with the right status and `tail.txt`; `dismiss` removes it.

### M3b. Flow, input and query state machines (pure)

The three pieces the design specifies most tightly, as pure Rust with an injected clock and a scripted transport, tested before any socket uses them.

- `flow`: whole-frame queue, 4 MiB live cap, backpressure signal, stall after 5 seconds with frames pending, drop of unstarted frames only, epochs, restore delivery ahead of live output, `too_slow` after 3 stalls in 10 minutes.
- `input`: FIFO of items, 1 MiB cap with atomic `queue_full`, replies ahead of unstarted items, never into a partly written item, 100 ms hold for a split trailing sequence while a query is outstanding, `paste` (bracketed only when mode 2004 is on) and `submit` (one `\r`).
- `queries`: ownership on restore enqueue, outstanding list with last-byte positions and recorded cursors, reply matching, answers on stall drop, detach and takeover, departing client input drained first, 2-second expiry, the reply-shaped keystroke case.
- Tests: scripted scenarios with exact expected output bytes and input bytes, including every transition in the design's query rules, a partly written send that a reply must not split, a reply split across two reads, expiry, and each unanswered case the design names. Property tests check that no scenario produces two replies to one query.

### M3c. Attach

- `client`: raw mode, non-blocking relay in both directions, at most one frame buffered, SIGWINCH to resize frames, detach key in legacy and CSI-u forms or `none`, takeover prompt and in-process re-attach with the design's restore modes, exit with the program's status.
- Supervisor: `Attach`, `Attached`, `Busy`, fork of the program after the creator's attach (30-second timeout), takeover, `--if-detached`, restore through `model` and `flow`, exit while attached (drain, `exited` frame, directory removed), nesting refusal, `PERCH_SESSION_ID` checked with `getsid`, `detach`.
- CLI: `new` (attaching), `attach`, `detach`.
- An end-to-end harness: a Rust test client speaks the protocol to a real supervisor and writes everything it receives to a file. The M2b Node harness takes that file as a stream with resize events and checks the screen.
- Tests:
  - New, attach, detach, reattach; output while detached appears; `screen` and `full` modes end-to-end through the harness.
  - Takeover: the old client gets `detached`, later input is discarded; `--if-detached` gets `Busy` and exit 75.
  - A creator that never attaches leaves nothing.
  - **No injection**: `stdin-recorder` runs as the program through attach, detach, takeover, restore in each mode, resize and kill, with no queries. It must receive exactly the bytes typed, nothing else.
  - Detach key, both forms, and `none`.
  - Model failure injected (via `test-hooks`) in feed, resize, dump and the parser: the program survives, output still flows, `info.model` is `failed`, the restore shows the notice and sends SIGWINCH, detached replies stop.

### M3d. Send, queries and flow end to end, environment, status

- Wire `input`, `queries` and `flow` into the supervisor. `send` op and CLI. Linked variables. `info` live fields, memory, cwd, foreground. `perch doctor`.
- Tests:
  - `dsr-timer`: detached replies under 50 ms; exactly one reply across attach, takeover, stall and detach at scripted points; the unanswered cases behave as the design says.
  - `reedline-prompt` (`reedline =0.52.1`, default features off, `crossterm 0.29`, as Raven pins them) runs `Reedline::read_line` in a loop. Detached, fed 200 lines by `send`, plus one 900 KB send: no prompt waits 2 seconds.
  - Reply priority with a write gate: a reply arriving before a large send starts goes first; one arriving while the send is partly written goes after it. The same with the terminal's reply split across two reads.
  - Send: `queue_full` leaves nothing queued; send bytes never interleave with typed bytes; `paste` with 2004 on and off; `submit` adds exactly `\r`; the control connection dropped before `queued` gives `outcome_unknown` at the client and no resend, and dropped after it gives `queued`.
  - Flow: a client that stops reading holds the program at most 5 seconds; a slow client slows it without drops; keystrokes and resizes reach the program while the client's output is blocked; stall mid-restore and mid-live end with a correct screen in the harness; `too_slow` after 3 stalls.
  - Linked variables: two `ssh-agent`s, `ssh-add -l` inside the session reaches whichever client is attached; a missing variable on attach leaves a dangling link.
  - `doctor` output on both OSes.

### M4. Linux host checks

`tests/linux/` builds a disposable privileged container from `ubuntu:24.04` with systemd as PID 1, `openssh-server` and a test user. It runs locally under colima (arm64) and in CI on both Linux runners. If logind does not work inside the container, the scripts fall back to a separate colima profile created for the test and deleted after it. The user's own VM is never changed.

- **SSH drop**: ssh in as the test user, start a session, kill the client's sshd process, reconnect, attach, and check the program kept running and the screen restored.
- **logind**: with `KillUserProcesses=no`, `loginctl terminate-session` leaves the session alive. With `yes`, it ends it, and `perch doctor` warned beforehand.
- **tmpfiles**: an aging rule of 0 seconds on the runtime directory; `systemd-tmpfiles --clean` keeps a live session and removes a dead one.
- **Binary deleted and replaced**: start a session from a binary in a temporary directory, delete the directory, attach with a binary at another path. This is what an extension update does. A server restart is the creator's process tree dying, which M3a covers.
- **Old and minimal userlands**: the static musl binary creates, attaches to and kills a session on `rockylinux:8` and `alpine:3`. arm64 runs locally and on `ubuntu-24.04-arm`; x64 runs on `ubuntu-24.04`. `file` shows a static executable with no interpreter.

### M5a. Extension: binary, list, kill

- Thin adapter: `src/core/` has no `vscode` import and holds recognition, restore sets, retries, list diffs, label building and settings logic, unit-tested in plain Node. `src/vscode/` wires it.
- Binary resolution, chmod on activation, `perch.path` (machine scope, restricted in untrusted workspaces), `available()` false on Windows hosts.
- `perch self-install <dest>`, a hidden Rust subcommand: exclusive `flock` on a lock file, newer-wins comparison under the lock, temporary file and rename, and a fallback without `flock`. Tested in Rust with concurrent processes and with `flock` disabled through `test-hooks`. The extension calls it with `<globalStorage>/bin/<os>-<arch>/`, then `environmentVariableCollection.append('PATH', ...)` and `PERCH_BIN`, under `perch.addToPath`. The command "Perch: Show Command-Line Path".
- Session list view (host in the title, kind icons, descriptions, exited and lost entries with "Show last output" and "Dismiss"), refresh on activation, focus and visibility, 5 seconds visible and 30 seconds hidden. Kill with the confirmation modal. Status bar count and quick pick.
- Labels: `<workspace-key>#<window-key>`, where the workspace key is the first 16 hex digits of the SHA-256 of the workspace file or folder URI, or `none`, and the window key is 8 random hex digits per activation.
- Tests: plain-Node units for the core; test-electron for activation, the view listing a CLI-created session, kill confirmation (mocked modal), `available()` with a mocked platform.

### M5b. Extension: terminals

- Open (with the "Move it here?" confirmation), creation flow (`prepareLaunch`, `createTerminal` with `isTransient`, readiness wait, exec errors), the persistent shell profile with `perch.shell`, recognition in `onDidOpenTerminal` and at activation, adoption, restore (`info` first, `--if-detached`, exit 75 confirmed with `info`, 60-second retries), exit reasons, first-close notice, `--detach-key none` and `--restore full` everywhere.
- Settings: `perch.restoreTerminals`, `perch.linkedEnvironment`, `perch.shell`, `perch.path`, `perch.addToPath`, and `terminal.integrated.scrollback` passed to `perch new`.
- Tests:
  - Plain Node: recognition from `creationOptions`, restore-set changes per exit reason and code, retry policy, two windows racing (simulated with the CLI's `--if-detached`).
  - test-electron: create a session and get its terminal; close the terminal and the session lives; reopen from the list and the screen is restored; a program that really exits with 75 is dropped from the restore set.
  - Cross-process restore: two `runTests` launches sharing `--user-data-dir` and the workspace. The first creates sessions and writes a phase marker file; the second, on activation, must restore each one exactly once.
  - Adoption after an extension-host restart cannot be tested in test-electron, because the runner lives in the extension host. It is covered by the plain-Node adoption tests and listed in `verification.md` as not run.

### M5c. Extension: API and environment checks

- API v1 exactly as the design's interface, `onDidEndSession`, the `perch doctor` warning once per host.
- Tests: `getApi(1)` returns the API, `getApi(2)` returns `undefined`; `createSession` then `send` while the terminal is closed then `kill`, with `onDidEndSession`; `send` error codes, including `outcome_unknown`; `prepareLaunch` with a `TerminalProfileProvider` and `sessionForTerminal`.
- Linked `VSCODE_IPC_HOOK_CLI`, local check in test-electron: a session's `$VSCODE_IPC_HOOK_CLI` is the link, the link resolves to the window's socket, and `connect()` succeeds. **Default rule**: `SSH_AUTH_SOCK` ships as a default. `VSCODE_IPC_HOOK_CLI` ships as a default only if this check passes, and the docs say it is not verified over Remote-SSH.

### M6. Packaging and release

- Builds: `x86_64-unknown-linux-musl` on `ubuntu-24.04`, `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`, `aarch64-apple-darwin` on `macos-15`, `x86_64-apple-darwin` on `macos-15-intel`, with `MACOSX_DEPLOYMENT_TARGET=11.0` for darwin and ad-hoc signatures. Each binary runs `perch new -d`, `list`, `kill` on its own runner. Linux binaries are checked with `file`.
- Seven VSIXs: six targets (the musl binaries also serve `alpine-x64` and `alpine-arm64`) and a universal one without a binary. A check unpacks each and asserts the binary's presence and architecture, not its mode bits (the extension sets those).
- Smoke test per runner: `code --install-extension <vsix> --extensions-dir <tmp>` and run the installed binary.
- Publishing to the Marketplace and Open VSX, gated on a tag and on secrets, is written but not run, because it needs registry tokens. Whether an ad-hoc signature is enough for a VSIX downloaded from a registry, as opposed to one built in CI, stays in `verification.md` as not run.

### M7. Documentation

README, `docs/usage.md`, `docs/api.md` (integration guide with the Raven and Sight guidance), `docs/cli.md`. Reviewed by Sonnet and Terra until both agree, then merged.

## Verification matrix

| Claim from the design's list | Milestone | How |
|------------------------------|-----------|-----|
| Fidelity contract | M2b, M3c | xterm.js harness with per-feature oracles; tmux text and SGR |
| Transitions (stalls, takeover mid-restore, split replies) | M3b, M3d | Pure scripted tests; end-to-end harness |
| Parser agreement with avt | M2a | Counterexamples and proptest differential |
| DECSTR, `3J`, `?1049l` on primary | M2b | xterm.js and tmux automated. Terminal.app and iTerm: not run |
| Reedline detached and behind a large send | M3d | `reedline-prompt` with Raven's pins |
| Linked variables | M3d, M5c | Two agents; local `code` IPC check. Remote-SSH: not run |
| Survival: window close, SSH drop, server restart, update | M3a, M4, M5b | Process-group kill; container SSH drop; binary deleted and replaced; terminal close |
| logind | M4 | Systemd container or dedicated colima profile |
| Directory `flock`, tmpfiles skip | M3a, M4 | Both OSes; container |
| `creationOptions` on adopted and profile terminals | M5b, M5c | test-electron for profile and created terminals; restart adoption not run |
| Registry installation per local and remote combination | M6 | Local VSIX install smoke tests. Registry installs: not run |
| Ad-hoc signed darwin binaries | M6 | Run on matching runners. Registry quarantine: not run |
| `PERCH_SESSION_ID` and `getsid` | M3c | Integration test |
| Keystrokes under backpressure | M3d | Integration test |
| Memory after widening | M2b | RSS bound test |
| Reply behind a large send | M3b, M3d | Pure and write-gated tests |
| Concurrent binary copies | M5a | Rust concurrent `self-install` test, with and without `flock` |
| Shell integration after reattach mid-command | M5b | Not automatable here; listed as not run |
| No injection (brief) | M3c | `stdin-recorder` invariant test |

## Fuzzing

`fuzz.yml` runs weekly and on demand, not on PRs. It installs a pinned nightly with `rustup toolchain install nightly-2026-10-15`, installs `cargo-fuzz`, and runs each target for 10 minutes with AddressSanitizer: `model_feed` and `restore` (calling avt and `vt` directly, without `catch_unwind`, so a panic is a finding) seeded with the fixture corpus, and `frame_decode`. Crashes are uploaded as artifacts. Locally, fuzzing needs a rustup nightly, which this laptop's Homebrew toolchain does not provide, so it is CI-only.

## Risks to the plan

- **avt bugs found by fixtures.** Fix upstream (Apache-2.0) and pin the release. If upstream is slow, use a `[patch]` to a fork at a fixed commit, which `deny.toml` allows by name.
- **VS Code test flakiness.** Tests poll for state with timeouts instead of sleeping.
- **Electron in the local sandbox.** test-electron may not run locally. CI is the source of truth for those tests, and the plain-Node core keeps local iteration fast.
- **logind in a container.** If neither the container nor a dedicated colima profile can run logind, that check is recorded as not run.

## Review history

### Revision 2

From the first review by Sonnet and Sol: split M2, M3 and M5 into reviewable PRs; moved the query, input and flow state machines ahead of the supervisor as pure code; replaced `vte` with Perch's own parser after Sol's counterexample; action-based fixtures and per-feature oracles that say what xterm.js cannot observe; an end-to-end harness; tests for every design item that lacked one, including no injection and model failure; `perch self-install` because Node has no `flock`; a restart-aware restore test; fuzzing as a pinned-nightly job; a concrete build and run matrix; Linux host checks in a disposable systemd container; honest "not run" items in `verification.md`; npm license policy; VS Code 1.82 tested, not just declared.
