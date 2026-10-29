# Perch implementation plan

Status: draft for review. This plan implements [design.md](design.md). Where they disagree, the design wins and the plan gets fixed.

## Principles

- **Riskiest first.** Restore and the query rules are where the design can be wrong. They are built and proven, against a real terminal emulator, before anything depends on them.
- **One PR per milestone**, each one merged before the next starts. Each PR leaves `main` building, tested and usable as far as it goes.
- **Proof in every PR.** Each milestone lists the tests that must exist and pass, and what is checked by hand. CI runs the automated ones on Linux and macOS.
- **Small dependency set.** Rust: `avt` (pinned `=0.19.0`), `vte`, `libc`, `serde`, `serde_json`. No async runtime, no CLI-parsing crate unless argument parsing grows past a page, no `nix` (direct `libc` calls in one `sys` module keep the unsafe code in one place). Every dependency must be MIT, BSD, ISC, Apache-2.0 or another GPL-3.0-compatible license, checked by `cargo deny` in CI.
- **License headers.** Every source file starts with `// SPDX-License-Identifier: GPL-3.0-only`. `Cargo.toml` and `package.json` declare `GPL-3.0-only`.

## Repository layout

```text
Cargo.toml                       workspace, release profile with panic = "unwind"
deny.toml                        cargo-deny license and advisory policy
crates/perch/
  src/main.rs                    CLI dispatch
  src/cli/                       one file per subcommand
  src/sys.rs                     every libc call: pty, fork, flock, termios, peer creds, proc info
  src/runtime.rs                 runtime dir checks, publish, list, cleanup
  src/meta.rs                    meta.json schema and atomic writes
  src/proto.rs                   frames, Hello/Welcome/Attach, control ops
  src/supervisor/                event loop, clients, input queue, flow control, kill
  src/model/                     avt wrapper, UTF-8 decoder, mode parser, restore builder, queries
  src/client.rs                  attach client: raw mode, relay, detach key, takeover prompt
  src/env_links.rs               linked variables
  src/procinfo.rs                session scan, memory, cwd, start times (Linux and macOS)
  tests/                         integration tests driving real PTYs
crates/perch/fuzz/               cargo-fuzz targets for the model and frame parser
tests/fidelity/                  Node harness: @xterm/headless comparison
editors/vscode/                  the extension (TypeScript, esbuild, @vscode/test-electron)
docs/                            design, plan, user docs
.github/workflows/               ci.yml, release.yml
```

## Milestones

### M0. Design and plan (docs only)

Branch `docs/design`. Adds `docs/design.md`, `docs/plan.md`, `docs/research/emulator-spike.md`, and links them from the README.

Proof: reviews by Sonnet, Terra and Sol recorded in the design's review history.

### M1. Skeleton and CI

- Cargo workspace, `crates/perch` with `perch version [--json]`.
- `editors/vscode` skeleton: `package.json` (`extensionKind: ["workspace"]`, `engines.vscode` `^1.82.0`, the same floor as Raven; every API the design uses (`TerminalExitReason`, `creationOptions`, `isTransient`, `environmentVariableCollection`, terminal profile providers) is in the 1.82 typings), esbuild bundle, ESLint, an activation test that runs in VS Code via `@vscode/test-electron`.
- `ci.yml`: on Ubuntu and macOS, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo deny check`; for the extension, typecheck, lint and tests (Linux under `xvfb-run`).

Proof: CI green on the PR. `perch version` runs.

### M2. Model and restore, proven against xterm.js

The library half of the design's riskiest part, with no processes involved.

- `model::Utf8Decoder`: incremental, holds up to 3 bytes, U+FFFD per maximal invalid sequence.
- `model::ModeParser` on `vte`: the modes in the design's "Input modes" list, per-screen kitty stacks, OSC 0, 2 and 7, control-string state, RIS and DECSTR resets, query detection with byte offsets, reply-shape detection for client input.
- `model::Model`: wraps avt with `catch_unwind` on every call. `feed(bytes) -> Vec<Event>` where events are query positions and the DSR 6 cursor at each. Splits each read at query offsets so the cursor is the one at the query. Tracks whether both parsers are at ground. Scrollback length set at creation (default 1,000, ceiling 10,000). Size clamp 1..=500.
- `model::restore`: builds the prologue and the `full` and `screen` restores exactly as the design orders them, with history trimmed by client scrollback rows and the 4 MiB cap, and the ground-state rule plus the fallback (1 second idle or 5 seconds total) that feeds avt a `CAN` and withholds the rest of the unfinished sequence from model and client.
- Test binary `perch-model-dump` (dev only, behind a feature): reads a byte stream and a client size, writes the restore bytes and the model's expected cells as JSON.
- `tests/fidelity`: a Node harness with `@xterm/headless` (MIT). For each fixture it feeds the original stream into terminal A, feeds the restore into terminal B, and compares B with A cell by cell (character, colors, attributes), along with the cursor, the modes xterm.js exposes, and the scrollback lines up to the limit. It also runs each fixture through a second restore into a terminal left in a hostile state (alternate screen, margins, insert mode, a half-written CSI and an open OSC), to prove the prologue.
- Fixtures: recorded outputs of R (plain and reedline-style prompt redraws), zsh and fish with right prompts, `less`, `vim` enter and exit, progress bars with `\r`, wide characters, split UTF-8 at every offset of a sample, invalid bytes, RIS, DECSTR, kitty push and pop on both screens, OSC 8 left open, and resizes from 200 to 80 columns and back.
- Rust unit tests for the decoder and parser. A differential test runs avt's parser and Perch's mode parser over fuzzed input and checks they agree on where sequences end. `cargo-fuzz` targets for `Model::feed` and the restore builder (run in CI for 60 seconds each).
- The emulator sits behind the internal interface (`feed`, `resize`, `cursor`, `screen`, `history`).

Proof: the fidelity suite passes in CI. Any fixture that cannot pass is either a fix or an amendment to the design's "Not covered" list, recorded in the PR.

### M3. Supervisor, protocol and the core CLI

- `sys`: `posix_openpt`, `grantpt`, `unlockpt`, termios get and set, `flock` on directory fds, `SO_PEERCRED` and `getpeereid`, `utimensat` with `AT_SYMLINK_NOFOLLOW`, close-on-exec everywhere.
- `runtime`: directory checks (`lstat`, owner, 0700), `$PERCH_RUNTIME_DIR`, publish by build-then-rename, `list` with lock-then-read, `lost` rewriting under an exclusive lock with inode revalidation, removals by rename to `.trash-*`.
- `meta`: the schema with the frozen fields, atomic writes, start times.
- `proto`: frame codec (4-byte length, type, payload, 1 MiB cap), `Hello`, `Welcome` with capabilities, `Error`, `Attach`, `Attached`, `Busy`, control ops `info`, `kill`, `rename`, `annotate`, `detach`. 5-second `Hello` deadline.
- `supervisor`: double fork, `setsid`, umask and cwd recording, readiness pipe, exec-result pipe, fork after the creator's attach (30-second timeout), the `poll` loop, single live client with takeover and `--if-detached`, the 60-second identity check and hourly touch, SIGTERM as kill, kill scope (session scan, SIGHUP plus SIGCONT, SIGKILL after 5 seconds), tombstones on exit while detached (`tail.txt`).
- `client`: raw mode, relay with non-blocking I/O both ways, SIGWINCH to resize frames, detach key (legacy and CSI-u forms) with `none`, takeover prompt and in-process re-attach, exit with the program's status.
- CLI: `new`, `attach`, `list`, `info`, `kill` (with `--pid` recovery and start-time checks), `dismiss`, `rename`, `annotate`, `detach`.
- Model integration: output feeds the model; attach sends the restore for the requested mode.

Tests, all with real PTYs and real processes:

- New, attach, detach, reattach; output while detached appears on reattach.
- The program's umask, directory and signal mask match the creator's.
- Exec failure reports the error and leaves no session.
- A creator that never attaches leaves no session after 30 seconds (with the timeout shortened by an environment variable for tests).
- Takeover: the old client gets `detached`, its later input is discarded; `--if-detached` gets `Busy` and exit 75.
- Kill reaches background and stopped jobs of a shell (`sleep 1000 &`, a stopped `vi`), and a `SIGHUP`-ignoring child gets SIGKILL after 5 seconds.
- Liveness: SIGKILL the supervisor; `list` shows `lost` and keeps the log. `kill --pid` refuses a PID whose start time does not match.
- Runtime directory: refuse a symlink, a foreign owner, a wrong mode. Delete a live session's directory; the supervisor republishes within 60 seconds and marks it degraded.
- Concurrency: 20 parallel `list` runs during 10 parallel `new` runs never remove a live or starting session.
- Survival: start a session from a process group, then SIGHUP and kill that whole group and its parent; the session lives (this is what VS Code's pty host and sshd do).
- Version handshake: a client offering only an unknown version gets `Error` with both ranges.

Proof: the tests above in CI on Linux and macOS, plus a manual run in Terminal.app recorded in the PR.

### M4. Input, queries, flow control and the environment

- Input queue: FIFO of items, 1 MiB cap, `queue_full`, reply priority for supervisor replies and recognized client replies, never inside a partly written item. `send` with `paste` and `submit`. CLI `send`.
- Query ownership as the design specifies: ownership on restore enqueue, outstanding list with positions and recorded cursors, reply matching, answers on stall drop, detach and takeover, 2-second expiry, draining a departing client's input.
- Flow control: whole-frame queue, 4 MiB live cap, backpressure, 5-second stall with drop and resync, epochs, `too_slow` after 3 stalls in 10 minutes, restore delivery ahead of live output.
- Linked variables: `env/` symlinks, re-pointing on attach, missing values.
- Status: `info` live fields, memory and cwd from `procinfo`, foreground process, `last output`.
- `perch doctor`.

Tests:

- A test program that writes DSR 6 and DA1 and times how long the reply takes: under 50 ms while detached, from the terminal while attached, and exactly one reply across attach, takeover, stall and detach at every point of a scripted sequence.
- A reedline-based test program (a dependency of the test crate only) that prints a prompt after each line it receives: detached and fed lines through `send`, it never waits 2 seconds.
- Reply priority: a 900 KB `send` queued while a DSR 6 is answered; the program sees the reply before the paste. The same with the terminal's reply split across two client reads.
- Flow control: a client that stops reading; the program blocks for at most 5 seconds, then runs on; resuming the client gives a correct screen. A slow client slows the program without drops. Keystrokes reach the program while the client's stdout is blocked.
- Linked variables: `ssh-add -l` inside a session reaches the agent of whichever client is attached (with two `ssh-agent`s), and the `code` IPC socket by a fake server.
- `doctor` on Linux reports cgroup, linger and `flock` support.

Proof: tests in CI on both platforms.

### M5. Linux host checks in containers

Scripts under `scripts/linux-checks/` run in Docker (and in CI on the Ubuntu runner):

- Static musl build for x64 and arm64 runs on an old-glibc image (CentOS 7 or Rocky 8).
- systemd-tmpfiles: with an aging rule of 0 seconds on the runtime dir, `systemd-tmpfiles --clean` keeps a live session and removes a dead one.
- logind behavior is documented, not automated. `KillUserProcesses` needs a full systemd login session, which a container does not provide. The PR records a manual check on a Linux VM if one is available, and otherwise says it was not checked.

Proof: the scripts' output in CI.

### M6. The extension

- Binary resolution: bundled `bin/perch`, chmod on activation, `perch.path` for development (machine scope, restricted in untrusted workspaces), `available()` false on Windows hosts.
- Stable copy in global storage under a lock, newer-wins, rename fallback; `environmentVariableCollection.append('PATH', ...)`; `PERCH_BIN`; `perch.addToPath`.
- Session list view with host in the title, icons per kind, descriptions, inline open and kill, exited and lost entries with "Show last output" and "Dismiss"; refresh triggers and the 5-second and 30-second cadences; status bar count and quick pick.
- Open with confirmation for a session attached elsewhere. Kill with the confirmation modal. Detach notice on first close.
- Persistent shell profile with `perch.shell`.
- Creation flow (`prepareLaunch`, `createTerminal` with `isTransient`, readiness wait). Recognition of Perch terminals by file name and arguments in `onDidOpenTerminal` and at activation. Adoption, then restore with `info` first, `--if-detached`, exit 75, 60-second retries, exit reasons.
- API v1 exactly as the design's interface, including `onDidEndSession`.
- `perch doctor` once per host, with a one-time warning.

Tests (`@vscode/test-electron`, run against a real binary built in the same CI job):

- Activation, `available()`, the view lists a CLI-created session.
- `createSession` returns a running session and its terminal; `send` reaches it while its terminal is closed; `kill` ends it and `onDidEndSession` fires.
- Closing a Perch terminal detaches; reopening from the list restores the screen.
- Reload: after `workbench.action.reloadWindow`, the terminal comes back once (no duplicate), and `terminals()` reports it.
- `prepareLaunch` with a `TerminalProfileProvider`; `sessionForTerminal` works on that terminal.
- `creationOptions.shellArgs` is populated for adopted terminals.

Manual checks recorded in the PR, with screenshots: VS Code on macOS, open, close the window, reopen, the R session is back with its screen; Remote-SSH to a Linux host if one is available (Docker container with sshd as a fallback), drop the connection, reconnect.

### M7. Packaging and release workflow

- `release.yml`: build `perch` for `darwin-x64`, `darwin-arm64` (ad-hoc signed), `linux-x64`, `linux-arm64` (static musl, also used for `alpine-x64` and `alpine-arm64`); package seven VSIXs (six targets plus a universal one without a binary) with `vsce package --target`; publish to the Marketplace and Open VSX with `vsce publish --packagePath` and `ovsx publish --packagePath`, gated on a version tag and on secrets the maintainer adds.
- A packaging check in CI that builds every VSIX and inspects that each contains the right binary with mode 0755.

Proof: the packaging check in CI. Publishing itself is not run from this work: it needs registry tokens. The PR says so and lists the steps for the maintainer.

### M8. Documentation

- README: what Perch is, install, first use, the CLI.
- `docs/usage.md`: the view, persistent shells, closing versus killing, restoring, linked variables, shell integration, `perch doctor`, platform limits (logind, cgroups, HPC reapers, containers, reboots, Stata license seats, `set more off`).
- `docs/api.md`: the API for integrating extensions, with the Raven and Sight guidance from the design.
- `docs/cli.md`: every command and option.

The docs go through their own review loop (Sonnet and Terra) before the final PR.

## Verification matrix

Each item from the design's "What the plan must verify" list, and where it is proven.

| Claim | Where | How |
|-------|-------|-----|
| Fidelity contract, transitions | M2, M4 | xterm.js harness; integration tests |
| tmux as a second terminal | M2 | Fixture replay inside `tmux` with `capture-pane -e` comparison, on macOS and Linux CI |
| vte and avt agree on boundaries | M2 | Differential fuzz test |
| DECSTR, `ESC [3J`, `?1049l` on primary in other terminals | M2 | xterm.js and tmux automated; Terminal.app and iTerm by hand, recorded |
| Reedline at a detached prompt, behind a large send | M4 | Reedline test program |
| Linked variables | M4, M6 | `ssh-agent` test; `code` IPC by hand in VS Code |
| Survival: window close, SSH drop, server restart and update | M3, M6 | Process-group kill test; manual VS Code and Remote-SSH check |
| logind `KillUserProcesses` | M5 | Manual on a VM if available, else documented as unchecked |
| `flock` on directories, tmpfiles skip | M3, M5 | Unit test on both OSes; tmpfiles container script |
| `creationOptions` on adopted and profile terminals | M6 | Extension tests |
| Installation per registry and remote combination | M7 | Packaging check only; registry installs need a published version, listed for the maintainer |
| Darwin binaries with ad-hoc signature | M7 | Run the CI-built binary on this Mac |
| `PERCH_SESSION_ID` validated with `getsid` | M3 | Unit test |
| `?1049l` harmless on primary | M2 | Harness fixture |
| Keystrokes under backpressure | M4 | Integration test |
| Memory after widening deep history | M2 | Test measuring RSS after a resize to 500 columns |
| Reply behind a large send | M4 | Integration test |
| Concurrent binary copies | M6 | Extension unit test with two copiers, with and without `flock` |

## Risks to the plan

- **Fidelity fixtures may expose avt bugs.** If a bug blocks the contract, the fix goes upstream to avt (Apache-2.0) and Perch pins the fixed release, or the design's "Not covered" list grows, recorded in the PR.
- **VS Code test flakiness** around terminals and reloads. The extension tests poll for state with timeouts rather than sleeping.
- **No Linux VM or remote host here.** Container checks cover what they can. Remote-SSH, logind and registry installs are recorded as manual checks or as unchecked, never claimed.
