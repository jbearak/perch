# Perch design

Status: draft for review, revision 5. This document turns [brief.md](brief.md) into decisions. Each section says what Perch does and why, and names the alternatives it rejected. [Review history](#review-history) lists what each review round changed.

## What Perch is

Perch has two parts.

- `perch`, a small Rust binary for macOS and Linux. It starts a session, keeps it alive with no client attached, and lets clients list, attach to, send to and kill sessions.
- A VS Code extension (`jbearak.perch`) that bundles the binary, shows the session list, opens sessions in terminals, provides a "Persistent shell" terminal profile, and gives Raven and Sight a versioned API.

A session is one program (R, Stata, a shell, anything) running on a pseudo-terminal that Perch owns. Closing the editor window, losing the SSH connection or restarting the VS Code server does not end it, as long as the host's process policy lets a user process outlive its login (see [What can still end a session](#what-can-still-end-a-session)). Rebooting the host, or killing the supervisor process, does end it.

Perch never writes anything into a session on its own. Bytes reach a session's input only from an attached terminal, from an explicit `send` by the user or an integrating extension, or as a reply to a status query the program itself sent while no terminal was attached (see [Terminal queries](#terminal-queries)). A real terminal would send the same reply.

## Prior art and how Perch differs

| Tool | Model | State on reattach | Detached queries | Machine interface | License |
|------|-------|-------------------|------------------|-------------------|---------|
| tmux, GNU screen | Central server, full terminal emulator, re-renders everything to the client | Exact (it owns the screen) | Answered (it is the terminal) | Control mode (tmux `-CC`), format strings | ISC, GPL-3.0+ |
| dtach | One process per session, raw passthrough | None. Redraw by `^L` or SIGWINCH | Unanswered | None | GPL-2.0 (compared from docs only) |
| abduco | One process per session, raw passthrough | None. Redraw by SIGWINCH | Unanswered | None | ISC |
| shpool | Central daemon (systemd user service or autodaemonized), vt100 emulator per session | Screen restored from the emulator | Not documented | `list --json` | Apache-2.0 |
| zmx | One daemon per session, libghostty-vt emulator, multiple clients with a leader | Screen and scrollback restored from the emulator | DA1 and DA2 only | Plain text list, labels | MIT |
| mosh, Eternal Terminal | Network layer: roaming and reconnecting transport for one login | mosh resyncs its own screen model; neither keeps a session after its client is gone for good | n/a | None | GPL-3.0, Apache-2.0 |
| VS Code pty host | Editor keeps terminals during reload and short reconnects | Replays its own buffer | Answered while a window is connected | None for other extensions | MIT |

What Perch takes from these, and where it departs:

- **Passthrough while attached, like dtach and abduco.** Live output goes to the client byte for byte. VS Code's terminal keeps its own scrollback, selection, links and shell integration sequences, because nothing re-renders them. tmux and screen redraw everything through their own emulator. That costs native scrollback, adds a prefix key, sets `TERM` to `tmux-256color` or `screen`, and drops escape sequences tmux does not know about unless the user enables passthrough. Those costs are why Raven's docs today describe tmux as a workaround and not a solution.
- **A screen model on the side, like shpool and zmx.** dtach and abduco keep nothing, so a reattached terminal shows a blank screen until the program redraws, and line editors lose their mode state (bracketed paste, application cursor keys). Perch feeds all output through a terminal emulator that it uses only to restore the screen on attach and to answer status queries while detached. It never renders live output from it.
- **One supervisor per session, like dtach, abduco and zmx, and unlike tmux and shpool.** See [Process model](#process-model).
- **Answers cursor-position queries while detached.** zmx answers DA1 and DA2 so that fish does not wait 10 seconds at a prompt. Raven's console uses reedline 0.52, whose painter asks for the cursor position (DSR 6) when it draws a prompt, and crossterm waits up to 2 seconds for the reply. A detached Raven console that receives code over its socket would stall at every prompt. Perch answers DSR 5, DSR 6, DA1 and DA2 from its model when no terminal is attached.
- **No code injection.** zmx's `write` command types a `printf ... | base64 -d > file` pipeline into the shell. Perch has no feature that composes commands for the program.
- **A contract with clients that outlive versions.** Supervisors run for weeks. The extension that started them gets updated, and the update deletes its directory, binary included. Perch versions its wire protocol and its metadata, and it has a recovery path for a session no current client can talk to. tmux refuses to talk to a server of another protocol version, which strands the session until the user finds a matching binary. zmx is pre-1.0 and makes no promise.
- **Not a network layer.** mosh and Eternal Terminal solve roaming and lossy links for one login. VS Code Remote already owns the transport. Perch sits on the host and does not care how the editor reaches it.
- **Longer-lived than VS Code's pty host.** VS Code keeps terminals across a window reload and short disconnects, but not past the reconnection grace period, a server restart or a server update. Perch sessions survive all three.

### Why not build on an existing tool

- **tmux as a backend** (`tmux -L perch`, control mode). tmux is missing on many macOS machines and HPC nodes, versions vary widely, and the re-rendering costs above apply.
- **zmx as a backend.** It is the closest tool and is MIT-licensed. It is pre-1.0 with no cross-version guarantee, so the extension could not promise that a new release still reaches old sessions. It needs a Zig toolchain for six targets. It lacks DSR answers, machine-readable metadata (kind, owner, working directory, memory) and the placeholder and environment features below, and it ships the injection-based `write`. Perch borrows its ideas: per-session daemons, emulator-backed restore, query answers while detached. It does not borrow its code.
- **shpool as a backend.** It needs a long-lived daemon that is either a systemd user service (not available on macOS or many HPC systems) or autodaemonized. Upgrading that daemon means restarting it, which ends every session or blocks the upgrade.

## Process model

**Decision: one supervisor process per session, no central server.** The brief proposed this, and it holds up.

- A crash or leak in one supervisor affects one session.
- There is nothing to install, start or upgrade. Old and new supervisors coexist, each running the binary that started it.
- `list` reads a few small files per session. With tens of sessions this takes milliseconds.

The cost is that cross-session features (one socket for every session, server-wide options) are harder. Perch needs none.

### Starting a session

`perch new` forks. The child calls `setsid()` and forks again so it cannot reacquire a controlling terminal. The grandchild becomes the supervisor:

- It sets `umask(077)`, ignores SIGHUP and SIGPIPE, changes directory to `/`, points stdin and stdout at `/dev/null`, and sends stderr to `supervisor.log` in the session directory.
- Every descriptor it opens is close-on-exec. The program never inherits the session lock, the socket or the PTY master.
- It creates and publishes the session directory (see [Files and sockets](#files-and-sockets)) and starts listening.
- It reports readiness, or the reason it failed, to the original process over a pipe. `perch new` returns only once the socket is listening.

The PTY is created with the window size and termios of the terminal `perch new` runs in, including `IUTF8`, so line editing of UTF-8 input works from the first prompt. With `-d` (detached) there is no terminal, so Perch uses 80 by 24 and sane defaults.

**When `perch new` is going to attach** (the default, and how the extension creates every session), the supervisor forks the program only after that client's attach handshake. The program's first output then goes to a live terminal, and its startup queries are answered by the real terminal, not by Perch. If the attach handshake has not arrived 30 seconds after readiness (the creating process died, or the user closed the terminal at once), the supervisor removes its directory and exits without starting the program.

The program runs in a new session with the PTY slave as its controlling terminal. The supervisor records the creator's umask and working directory (or `--cwd`) before changing its own. Before `exec`, the child restores that umask, changes to that directory, clears the signal mask, restores default signal dispositions, and closes every descriptor except 0, 1 and 2, plus one close-on-exec pipe back to the supervisor. Resource limits and niceness are inherited from the creator unchanged. If `exec` fails, the child writes the error to the pipe. If it succeeds, the pipe closes with nothing written. Only then does the supervisor record `state: "running"`. An exec failure ends the session at once with the error, which the attached creator prints.

`running` means the program started. It does not mean the program is ready for input. Readiness that matters to an integration, such as Raven's console socket accepting connections, is the integration's own check.

The supervisor then runs one single-threaded `poll()` loop over the listening socket, the PTY master, client connections, a self-pipe for SIGCHLD and an hourly timer.

### What can still end a session

The double fork changes a process's parent and session. It does not move it out of its cgroup, and some host policies stop whole cgroups. Perch's promise is conditional on host policy, and it says so.

- **systemd-logind with `KillUserProcesses=yes`.** When a login session ends, logind stops that session's scope and everything in it, `setsid` or not. Most distributions ship `no`. Some HPC sites set `yes`.
- **Services with `KillMode=control-group`.** A process started from a systemd unit (a `code tunnel` user service, for example) is killed when that unit stops or restarts.
- **HPC login-node reapers** that kill long-lived or high-memory processes.
- **Containers.** A Dev Container that stops when its window closes takes its sessions with it.
- **Flatpak and Snap editors running locally.** The sandbox may not let processes outlive the editor. Remote hosts are unaffected.
- **Reboots**, and anything that kills the supervisor.

`perch doctor` reports what it can detect: the supervisor's cgroup from `/proc/self/cgroup`, `KillUserProcesses` and linger from `loginctl show-user`, whether the runtime directory checks pass, and whether `flock` works on a directory there (it may not if `$PERCH_RUNTIME_DIR` is on NFS). The extension runs it once per host and shows a one-time warning when sessions are not expected to survive logout. Starting supervisors in their own transient systemd scope (`systemd-run --user --scope`) would get around the first two cases on hosts where the user's systemd manager survives. That needs linger, which only the user or an administrator should enable. It is deferred until after v1.

### Supervisor robustness

- The terminal model is optional. Every call into it (feed, resize, dump, history) and into the mode parser runs under `catch_unwind`, and the release build keeps `panic = "unwind"`. If either panics, the supervisor marks the model as failed, logs it, and carries on with passthrough. Restore then shows a notice and asks the program to redraw with SIGWINCH, and detached query answers stop. A model bug must never cost the user their R session.
- Writing to a client never blocks the loop. A slow client can slow the program down, as any terminal can, but a client that has stopped reading holds it up for at most 5 seconds. See [Flow control](#flow-control).

## Files and sockets

**Decision: everything lives under one short, node-local runtime directory, `/tmp/perch-<uid>/`.** This rejects the brief's `~/.local/state/perch/`, for three reasons.

1. **Shared home directories.** On HPC clusters `$HOME` is shared across login nodes over NFS. A state directory there would show node A's sessions on node B, where all of them look dead. Sessions are node-local, so their records must be node-local too.
2. **Socket path length.** `sun_path` is 104 bytes on macOS and 108 on Linux. `/tmp/perch-1000/k3x9q2ab/sock` is 31 bytes. Home directories on clusters can be long enough that `~/.local/state/perch/<id>/sock` breaks.
3. **Not `$XDG_RUNTIME_DIR`.** logind removes `/run/user/<uid>` when the user's last login session ends. That is exactly when a Perch session needs to survive. tmux uses `/tmp` for the same reason.

`$PERCH_RUNTIME_DIR` overrides the location.

```text
/tmp/perch-<uid>/              0700, owned by the user
  <id>/                        0700, one per session; the supervisor holds flock on it
    sock                       0600, the supervisor's socket
    meta.json                  0600, metadata
    supervisor.log             0600, capped at 256 KiB
    env/                       linked environment variables
    tail.txt                   written when the program exits while detached
  .new-<random>/               a session being created, not yet visible
```

Every Perch process checks, with `lstat`, that `/tmp/perch-<uid>` is a real directory (not a symlink) owned by the current uid with mode 0700, before using it. tmux does the same. If another user created it first, Perch refuses and tells the user to set `PERCH_RUNTIME_DIR`. Inside it, Perch opens paths relative to directory descriptors with `O_NOFOLLOW`. The supervisor also checks the peer's uid on every connection (`SO_PEERCRED` on Linux, `getpeereid` on macOS).

### Lock, liveness and publishing

**The supervisor holds an exclusive `flock` on the session directory itself**, opened `O_DIRECTORY | O_CLOEXEC`, for as long as it runs. That one lock does three jobs:

- **Liveness.** A free lock means no supervisor. This does not depend on PIDs, which get reused, or on the socket.
- **Protection from systemd-tmpfiles.** tmpfiles' aging skips any directory or file that holds a BSD lock, along with everything beneath it (tmpfiles.d(5)). That covers Linux.
- **One identity.** The lock is on the directory, not on a file inside it, so there is never a second unlocked lock file next to a live session.

**Publishing.** The supervisor builds a session in `.new-<random>/`: it takes the lock, binds the socket and writes `meta.json`. Then it renames the directory to `<id>/`. `rename` is atomic, so a session directory is always either fully formed and locked or not visible at all. Directory `flock` works on Linux and macOS, and on macOS it was checked to survive the `rename`.

**How `list` reads a session.** For each visible directory, `list` first tries a non-blocking shared lock, and only then reads `meta.json`. Reading after locking means a supervisor that records `exited` and releases the lock cannot be misread as `lost`.

- Lock not available: the supervisor is alive. Read the metadata and ask the supervisor for live fields.
- Lock acquired, `state` is `exited` or `lost`: a tombstone. Show it with `--all`.
- Lock acquired, `state` is `running`: the supervisor died. Upgrade to an exclusive lock, check that the path still names the same inode as the open descriptor, and rewrite the metadata to `state: "lost"`. The directory stays as a tombstone so `supervisor.log` survives for diagnosis.
- Lock acquired, `state` is `starting`, and the directory is older than 10 seconds: a supervisor that died while starting. Remove it.

**What may be deleted, and how.** `list` ignores names starting with `.` and never touches a directory younger than 10 seconds or one without `meta.json`. Removing anything (an expired tombstone, a dead starting session, a `.new-*` older than one hour) happens only under an exclusive lock on that directory, after checking that the path still names the locked inode. The directory is then renamed to `.trash-<random>` and deleted.

**Other cleaners.** macOS's periodic job removes `/tmp` entries untouched for 3 days, and administrators may run their own. A lock does not stop someone from unlinking a directory. So every 60 seconds the supervisor checks that its path still names the inode it holds, and that its socket is still the one it bound. Every hour it updates the timestamps of every entry in its directory, including files that other programs created there, such as Raven's console socket, with `utimensat` and `AT_SYMLINK_NOFOLLOW`. If its directory has gone, it republishes a fresh directory, socket and metadata by the same build-then-rename path. If something else now occupies the name, it logs the conflict and stays reachable only through the CLI's `--pid` recovery.

Republishing cannot bring back files that other programs owned in the old directory. A program that was listening on a socket there, such as Raven's console, has lost that socket for good, though the session itself keeps running. The supervisor records `"degraded": ["directory-recreated"]` in the metadata, the session list shows a warning on that session, and the integration must fall back (Raven types into the console through `send`, which its console accepts).

**IDs and names.** Session IDs are 8 random characters from `[a-z0-9]`. Each session also has a display name, set at creation and renamable later. The CLI accepts an ID, a unique ID prefix or an exact name.

### Metadata

`meta.json` is written atomically, by writing a temporary file and renaming it. Readers ignore fields they do not know.

```json
{
  "format": 1,
  "id": "k3x9q2ab",
  "name": "R: thesis",
  "kind": "r",
  "owner": "jbearak.raven",
  "annotations": { "raven.console": "ipc" },
  "command": ["/path/to/raven", "console", "--ipc", "/tmp/perch-501/k3x9q2ab/ipc"],
  "cwd": "/home/me/thesis",
  "host": "login2",
  "createdAt": "2026-10-29T15:04:05Z",
  "supervisorPid": 41200,
  "supervisorStart": 1761750245123,
  "pid": 41201,
  "pidStart": 1761750245130,
  "perchVersion": "0.1.0",
  "protocol": { "min": 1, "max": 1 },
  "state": "starting",
  "exit": null,
  "degraded": []
}
```

`state` is `starting` (published, program not started yet), `running`, `exited` or `lost`. `supervisorStart` and `pidStart` are the processes' start times as the kernel reports them (`/proc/<pid>/stat` on Linux, `proc_pidinfo` on macOS), used to tell a process from a later one that reuses its PID. `kind` is free text. The extension knows `r`, `stata` and `shell` and shows anything else as a generic program. `owner` is the extension ID that created the session, or absent. `annotations` is a string map that owners use to find their own wiring. Perch does not interpret it. Annotations are visible to every extension through the API, so owners keep secrets in their own 0600 files in the session directory, not in annotations.

Fields that change often (attached or not, last output time, memory) are not stored. `list` asks the supervisor.

## Wire protocol

Clients reach the supervisor over its Unix socket. Each frame is a 4-byte big-endian length, a 1-byte type and a payload. The length covers type plus payload and is capped at 1 MiB.

**Handshake.** The client's first frame is `Hello`, with JSON `{"min":1,"max":1,"client":"perch 0.1.0","mode":"control"|"attach","label":"..."}`. The supervisor drops a connection that sends no `Hello` within 5 seconds. It answers `Welcome` with `{"protocol":1,"perch":"0.1.0","capabilities":["send","annotate",...]}`, choosing the highest version both sides speak, or `Error` with both ranges. `capabilities` lists the operations it supports, so a newer client can check before calling instead of discovering the gap from an error.

**Control mode** carries JSON requests and responses, such as `{"id":7,"op":"info"}`. Unknown fields are ignored and unknown ops get an `unsupported` error. New operations and fields are added without a version bump. A protocol version bump is reserved for changes old peers would misread.

| Op | Request | Response |
|----|---------|----------|
| `info` | | Live fields: `attached`, `clientLabel`, `clientPid`, `lastOutputAt`, `lastInputAt`, `memoryBytes`, `foreground` (pid and name of the PTY's foreground process group), `rows`, `cols`, `state`, `exit`, `model` (`ok` or `failed`) |
| `send` | `data` (UTF-8 string) or `dataBase64`; `paste` (bool); `submit` (bool) | `queued`, or `queue_full` |
| `kill` | `force` (bool) | `ok`, then the session ends |
| `rename` | `name` | `ok` |
| `annotate` | `set` map, `unset` list | `ok` |
| `detach` | | `ok`; the attached client is told it was detached |

**Attach mode** starts with an `Attach` frame from the client, JSON: `{"rows":..,"cols":..,"restore":"full"|"screen"|"none","scrollback":..,"ifDetached":bool,"env":{...}}`, where `env` holds the client's values of the linked variables. The supervisor answers `Attached`, or `Busy` if `ifDetached` was set and another client is live. After that the frames carry raw bytes (input, output) and small JSON (resize, detached, exited). `Attach` follows the additive-fields rule.

**Timeouts.** Clients give each `info` call 500 ms. `list` queries sessions in parallel and shows one that does not answer in time as `unresponsive`, for example a supervisor stopped with SIGSTOP.

### Compatibility across versions

- **Frozen forever:** the frame format, `Hello`, `Welcome`, `Error`, the ops `info` and `kill`, the `meta.json` fields `format`, `id`, `state`, `exit`, `supervisorPid`, `supervisorStart`, `pid`, `pidStart` and `protocol`, and the same fields in `list --json`. Every future client can at least list and kill every session.
- **Additive:** everything else, announced through `capabilities`.
- **Protocol bumps.** If a bump is ever needed, new releases keep speaking the old version for at least 12 months after the release that introduced the new one. That is far longer than any session survives in practice.
- **Recovery.** If a client cannot talk to a supervisor at all, `perch kill --pid` signals the program's session and then the supervisor using the PIDs in `meta.json`. Before signaling a PID it checks that the process belongs to the user, that its start time matches the one recorded, and that the session lock is still held. A PID whose start time differs is a different process, and Perch leaves it alone.

## Sending input

`send` exists so that editors can send code while no terminal is attached, and so that sends from Sight do not depend on which terminal has focus.

- The supervisor keeps one FIFO input queue per session. Keystrokes from the attached client and `send` requests share it. A send's bytes are never interleaved with typed bytes.
- Replies to terminal queries go ahead of queued items, but never into the middle of an item already partly written. That covers replies the supervisor writes itself and client input that the supervisor recognizes as the reply to an outstanding forwarded query (see [Terminal queries](#terminal-queries)). Otherwise a reply queued behind a large send would reach the program after the pasted text, and the program would read the paste as its reply.
- The queue holds at most 1 MiB. A send that does not fit gets `queue_full` and nothing is queued. The supervisor writes the queue to the PTY master without blocking, as fast as the kernel accepts it.
- `queued` is the acknowledgement. Once a client has it, the bytes will be written in order unless the session ends first. If the connection drops before `queued` arrives, the outcome is unknown, and the client reports exactly that ("may not have been sent") and never resends. Raven's console socket has the same rule.
- `submit: true` appends a carriage return, which is what pressing Enter sends.
- `paste: true` wraps the data in bracketed-paste markers (`ESC [200~` and `ESC [201~`) only if the program has turned bracketed paste on, which the model tracks. Otherwise the bytes go raw. The caller decides; Perch does not guess.
- `send` has terminal semantics. The bytes arrive as if typed. Whether the program reads them as one line, several lines or a paste depends on the program and its terminal mode, as with any terminal. A long line sent to a program reading in canonical mode can be cut by the kernel's line limit. Callers that need reliable execution use an application-level channel, as Raven does with its console socket. Sight sends `do "<file>"`, a short line.
- Perch adds no bytes of its own. It does not add `set more off`, answer `--more--` or load anything.

## Attaching

**Decision: a terminal attaches by running `perch attach <id>` as the terminal's process.** The client puts its own terminal in raw mode and relays bytes between it and the supervisor socket. It forwards SIGWINCH as resize frames.

The alternative was a VS Code `Pseudoterminal` that speaks the protocol from the extension host. Rejected because:

- `perch attach` works from any terminal on the host, with or without VS Code. A user can SSH in from a phone and reach the same R session.
- Terminal replies (cursor position, device attributes, colors) come from the real terminal and flow back through the same pipe with no extra code.
- The protocol has one implementation, in Rust. The extension calls the CLI.

The cost is a second PTY hop, which tmux, dtach and abduco all have too.

**One client at a time; a new attach takes over.** The brief proposed this and it holds for v1. The old client receives a `detached` frame with the reason `takeover`. From then on, the supervisor discards anything that client sends. The client prints `[perch] Opened in another window. Press Enter to take it back, or close this terminal.` and waits. Multiple simultaneous clients (zmx's leader model) would need rules for whose size wins and whose keystrokes count. That is not worth it for an editor where the user is in one window at a time.

**Client label.** `Hello` carries a label made of two parts: a workspace key (the workspace file or folder URI, or `none` for an empty window) and a window key, random and new each time the extension activates. The CLI uses `tty:<ttyname>`. `info` reports the attached client's label, so the extension can tell "this workspace, earlier incarnation of a window" from "another window".

**Conditional attach.** `attach --if-detached` asks the supervisor to attach only if no client is live, decided atomically inside the supervisor. Automatic restores use it, so two windows restoring at once cannot take a session from each other. User-initiated opens take over, after confirmation when needed.

**Detach.** Closing the terminal detaches. So does `perch detach` from inside the session, and, in the CLI, the detach key `Ctrl-\`, the dtach and abduco default. `Ctrl-\` sends SIGQUIT in a cooked terminal, which kills R with a core dump, so intercepting it there removes a hazard. The extension passes `--detach-key none`, because closing the terminal already detaches, and Neovim users need `Ctrl-\ Ctrl-N`. The client recognizes the key in its legacy byte form and in the CSI-u forms that the kitty keyboard protocol and modifyOtherKeys produce.

**Nesting.** Inside a session `PERCH_SESSION_ID` is set. `perch attach` refuses to attach a session to itself.

### Flow control

A client can read slowly (xterm.js parses far slower than `cat` writes) or stop reading altogether (a frozen window, a dead SSH link, VS Code's pty host pausing output it has not had acknowledged). The first should slow the program down, as any terminal does. The second must not stall the program. So:

- Each client has an output queue of whole frames, capped at 4 MiB.
- **Slow client: backpressure.** While the queue is full but the client's socket is still accepting bytes, the supervisor stops reading the PTY master, as dtach and a plain terminal would. The program blocks on its next write until the client catches up.
- **Stalled client: drop and resync.** If frames are waiting and the socket accepts no bytes for 5 seconds, the client is stalled. An attached client with nothing pending is never stalled, however long it is idle. The supervisor finishes nothing it has not started: the frame partly written to the socket stays, so framing is never broken, and every frame not yet started is dropped. The supervisor marks the client as lagging, resumes reading the PTY into the model, and starts a new epoch. When the socket is writable again, it sends a resync (below) and resumes live output.
- So the program waits at most 5 seconds for a client that has stopped reading.
- Bytes already written before a drop may end inside an escape sequence or a UTF-8 character. The restore prologue handles that.
- 3 stalls within 10 minutes means this client cannot keep up. The supervisor detaches it with the reason `too_slow`, and the client says so and offers to reattach.
- `perch attach` reads frames from the socket only as fast as it can write them to its own terminal, and never buffers more than one frame. Otherwise flow control would move into the client's memory and the supervisor would never see the lag. It uses non-blocking I/O in both directions, so keystrokes (Ctrl-C above all) and resizes still reach the supervisor while its own output is blocked.

**Delivering a restore.** A restore is built in full when it is due and sent as a sequence of output frames ahead of any live output. The 4 MiB cap applies to live output, not to the restore. While a restore is being delivered, the supervisor does not read the PTY, so the program waits as it would behind any slow terminal, and the stall rule applies: if the client stops accepting bytes for 5 seconds, the rest of the restore is dropped with the live frames and a resync follows. A restore's size is bounded. The screen is clamped to 500 by 500 cells, so even an alternate screen dumped on top of a primary one with a color change on every cell stays under about 25 MB. History in a full restore is capped at 4 MiB, keeping the most recent lines.

### Restore on attach

**Decision: the screen model is [`avt`](https://crates.io/crates/avt)** (asciinema's virtual terminal, Apache-2.0, one dependency). The first draft chose `vt100`. A [spike](research/emulator-spike.md) showed why `avt` fits better:

| Check | vt100 0.18 | avt 0.19 |
|-------|------------|----------|
| Restore while in the alternate screen, then exit it | Not possible: only the active screen is serialized, and the primary is private | Exact: `dump()` restores both screens and both saved cursors |
| Narrower client (200 to 80 columns) | Truncates rows; the right-hand text is lost | Reflows the primary screen and its history; all five 190-character test lines survive |
| Snapshot taken mid-escape-sequence | Partial sequence lost | `dump()` ends with the unfinished parser state |
| Throughput | 111 MB/s | 92 MB/s |
| Memory, 10,000 lines of history at 120 columns | 20 MB | 20 MB |

The emulator sits behind a small internal interface (`feed`, `resize`, `cursor`, `screen`, `history`), so libghostty-vt or another emulator can replace it later. libghostty-vt is the most complete candidate, but it is pre-1.0 and needs Zig in the build for six targets. avt is pinned to an exact version.

**History** is avt's own primary-screen scrollback. Its length is set when the session is created: the extension passes `terminal.integrated.scrollback` (VS Code's default is 1,000 lines), and the CLI defaults to 1,000. The ceiling is 10,000 lines. avt stores every line at the full screen width and fixes the line limit when the model is created, so memory is about 17 bytes per cell: about 2 MB for 1,000 lines at 120 columns, about 20 MB for 10,000 lines at 120 columns, and at worst about 85 MB for 10,000 lines widened to the 500-column clamp. History lines reflow when the size changes. History is rebuilt from cells, not replayed from raw output, so it never contains queries, clipboard writes (OSC 52), shell integration marks or cursor-movement sequences.

**Model input.** avt takes characters, and the PTY produces bytes. An incremental UTF-8 decoder sits in front of avt. It holds up to 3 bytes of an incomplete character between reads and turns invalid bytes into U+FFFD, one per maximal invalid sequence. Its pending bytes are part of a restore's tail (below). The model assumes UTF-8. A program writing another encoding still reaches the terminal unchanged while attached, but its restored screen shows replacement characters.

The mode and query parser (below) reads the same bytes in the same order. Each read is processed in one pass: the parser finds the byte offsets of queries, the model is fed up to each query before Perch answers it, so DSR 6 reports the cursor where the program asked. RIS (`ESC c`) and DECSTR (`ESC [!p`) reset the parser's modes as they reset the model.

**Snapshot boundary.** A restore is always taken where both parsers are at rest: the mode parser in its ground state and the UTF-8 decoder holding no partial character. If a restore is due while the program is in the middle of a sequence, the supervisor feeds the model only up to the end of that sequence, takes the snapshot there, and sends the rest of that read as live output after the restore. avt's `dump()` then has no unfinished-sequence tail, and nothing is sent twice.

If the program stays in the middle of a sequence for 1 second with no further output, the supervisor takes the snapshot anyway, and that unfinished sequence belongs to the supervisor alone. The client never sees any part of it: Perch drops avt's dump tail whatever kind of sequence it is, and the mode parser swallows the rest of the sequence, up to its end, from that client's live output. The model still processes the sequence. If it turns out to be a query, the supervisor answers it, because the client never saw it. The UTF-8 decoder's partial bytes are handled the same way: the client never receives that character. The cost is that one sequence's effect (a color change, a title) is missing in the terminal until the program repeats it. That happens only when a program stops writing in the middle of an escape sequence, which is rare.

**Three restore modes.** The client chooses one in its attach frame.

- `full`: prologue, then history, then screen. The extension uses it for terminals it created, which start empty.
- `screen`: prologue, then screen, no history, and no erasing of the terminal's scrollback. The CLI's default, because `perch attach` typed into iTerm, Terminal.app or tmux must not wipe what the user already had there.
- `none`: used for the first attach of `perch new`. The program has not started, so there is nothing to restore.

A client that re-attaches in the same process, after "Press Enter to take it back", uses the mode it attached with, except that `none` becomes `screen`.

A **resync** after a stall is a `screen` restore. Its contract is weaker: lines that scrolled off while output was being dropped are missing from the terminal's scrollback.

**The prologue** puts any terminal into a known state. In order:

1. `CAN` (0x18), which aborts an escape sequence or control string that a dropped frame left open. A partial UTF-8 character ends as a replacement character at the next byte. `OSC 8 ; ; ST` then closes any open hyperlink.
2. If the client may be in the alternate screen, pop its kitty keyboard stack (`CSI < 99 u`), then leave the alternate screen (`ESC [?1049l`). Pop the main screen's kitty stack too. Kitty keeps a stack per screen.
3. DECSTR (`ESC [!p`), followed by explicit resets for terminals that implement DECSTR partly or not at all: `ESC [r`, `ESC [0m`, `ESC [4l`, `ESC [20l`, `ESC [?6l`, `ESC [?7h`, `ESC [?1l`, `ESC (B`, `ESC )B`, `SI`, `ESC [?25h`, `ESC >`, `CSI > 4 m` (modifyOtherKeys off), `l` for the private modes 9, 1000, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1016, 2004 and 2026, and default tab stops (`ESC [3g`, then `ESC H` every 8 columns).
4. Home the cursor and save it (`ESC [H ESC 7`), so the primary screen's saved-cursor slot holds the default that avt's dump assumes.

**Ordering around the dump.** avt's `dump()` assumes the terminal is in its default state when it draws, and it emits only the state that differs from default, at the end. So the state avt tracks (cursor keys, origin, autowrap, cursor visibility, insert and newline modes, character sets, tab stops, saved cursors, alternate screen) is reset by the prologue and set only by the dump. The modes only Perch tracks are set from the screen they belong to. The dump starts on the primary screen, so the primary screen's kitty keyboard stack is restored just before the dump. Everything else Perch tracks, including the alternate screen's kitty stack when the dump ends there, is set after the dump, and only where it differs from default.

A **full restore**, after the prologue:

1. Resizes the model and then the PTY to the client's size, clamped as above. If the size changed, the kernel delivers SIGWINCH and the program redraws as it would for any resize. If it did not change, Perch forces no redraw, because the model is already correct and forced redraws make some line editors print a second prompt.
2. Clears the screen and the terminal's scrollback (`ESC [H ESC [2J ESC [3J`).
3. Sends history as logical lines with SGR codes, joining soft-wrapped rows so the terminal wraps them at its own width. It sends the most recent history that fits in the client's scrollback limit, counted in rows at the client's width with wide characters counted as two columns, and that fits the queue cap. Then it scrolls the history just above the viewport with newlines, so that clearing the screen next does not erase it (erasing the screen in xterm.js does not move lines into scrollback).
4. Homes the cursor and clears the screen (`ESC [H ESC [2J`), restores the primary screen's kitty stack, sends avt's `dump()`, then the rest of the modes only Perch tracks.
5. Switches the client to live passthrough.

A `screen` restore does the same without steps 2 and 3.

**Input modes.** avt tracks cursor keys, origin, autowrap, cursor visibility, insert mode, character sets and the alternate screen. Its parser drops other private modes, so a small parser of Perch's own (the `vte` crate, Apache-2.0 or MIT) tracks the rest: bracketed paste (2004), mouse reporting (9, 1000, 1002, 1003) and encodings (1005, 1006, 1015, 1016), focus reporting (1004), alternate scroll (1007), synchronized output (2026), modifyOtherKeys (`CSI > 4 ; n m`), application keypad (`ESC =` and `ESC >`), the kitty keyboard flags stack for each screen (`CSI > u` push, `CSI < u` pop, `CSI = u` set, up to 16 entries each), the window title (OSC 0 and 2), the working directory (OSC 7), and whether the program is inside a control string. On RIS it resets everything. On DECSTR it resets what xterm.js resets: the modes above except title and working directory. The same parser finds terminal queries.

**Fidelity contract.** After a full restore into xterm.js, at the client's size:

- Each visible cell of the active screen has the model's character, foreground and background color (16, 256 or RGB), and bold, faint, italic, underline, blink, inverse and strikethrough. The cursor is where the model has it.
- After the program leaves the alternate screen, the primary screen is the model's primary screen.
- Every mode in the list above matches the program's last setting.
- History has the right text and colors in the right order, with no line duplicated or missing at the boundary with the screen and no blank row between them, up to the client's scrollback limit or 4 MiB of history, whichever is reached first. A logical line that continues from history onto the top row of the screen is split there: the terminal will not rejoin it if the user later resizes.

The contract applies to `full` restores. A `screen` restore meets the first three points.

Not covered: grapheme clusters (avt stores one character per cell, so combining marks and joined emoji lose their extra code points), underline styles and colors, hyperlinks (OSC 8), images (sixel, kitty graphics), double-width lines, the effect of a sequence left unfinished for more than a second (above), and any other terminal state neither avt nor the mode parser tracks.

The plan's first milestone tests this contract against xterm.js (headless), and against tmux as a second terminal, with Raven's console (reedline), zsh and fish with right prompts, `less`, `vim`, progress bars, split UTF-8, invalid bytes, partial control strings, RIS, kitty keyboard push and pop, and attaches at different widths. Restore and the query rules below are the riskiest parts of the design, so they are proven first.

### Terminal queries

Programs ask the terminal questions by writing escape sequences and reading the reply from their input. With no terminal attached, nothing answers.

**Who answers.** For each query, at most one party answers.

- Ownership passes to a client when the supervisor enqueues that client's restore. Snapshots are taken at a ground state, or else the unfinished sequence belongs to the supervisor (see [Restore on attach](#restore-on-attach)), so no query straddles the change. Every query the program wrote before that point is answered by the supervisor and is not in the restore, which is built from the model. Every query after that point reaches the client in its live output, and the real terminal answers.
- With no client, or a lagging client, the supervisor answers each query as it parses it.
- The supervisor keeps a list of queries forwarded to the live client and not yet answered. Each entry records the query's kind, its position in the client's output, and, for DSR 6, the cursor position at the moment of the query. It recognizes the client's replies by their shape (`CSI <r>;<c> R` for DSR 6, `CSI 0 n` for DSR 5, `CSI ? ... c` for DA1, `CSI > ... c` for DA2) with the same `vte` parser running over client input, and removes the oldest outstanding query of that kind. This matching is bookkeeping only. Client input always passes to the program unchanged.
- When the supervisor drops output after a stall, outstanding queries whose position falls in dropped frames never reached the client. The supervisor answers them at once, using the recorded cursor position.
- Before it detaches a client or lets another take over, the supervisor reads whatever that client has already sent, without blocking, and processes it as normal input. It then answers every query still outstanding, using the recorded positions, and from then on discards input from that client. A reply the old terminal sends later never reaches the program.
- A query the live client leaves unanswered for 2 seconds is dropped from the list without an answer, as with a real terminal that does not support it.

**Limits of the guarantee.** A keystroke that has the shape of a reply (Shift+F3 sends `CSI 1;2 R` in some terminals) can be counted as the answer to an outstanding DSR 6. If the client then detaches before the terminal's real reply, that query goes unanswered and the program waits for its own timeout. Perch never sends a second reply to a query it has seen answered. Duplicate replies are worse than a missed one, because a stray reply arrives as typed text.

**What it answers.**

| Query | Reply |
|-------|-------|
| DSR 5 (`ESC [5n`) | `ESC [0n` |
| DSR 6 (`ESC [6n`) | `ESC [<row>;<col>R` from the model |
| DA1 (`ESC [c`) | `ESC [?1;2c` (VT100 with advanced video) |
| DA2 (`ESC [>c`) | `ESC [>0;0;0c` |

The DA replies claim only a basic VT100, which the model emulates. Programs that probe for features (OSC 11 background color, kitty keyboard, XTVERSION) conventionally send DA1 last as a sentinel, so answering DA1 tells them "not supported" without Perch inventing anything. Perch does not answer color or feature queries. A wrong color answer could flip a program's light or dark theme. A program that waits for one of these without a DA1 sentinel will wait for its own timeout while detached, as it would in dtach or abduco.

## Environment

### Inside the session

The program starts with the environment of the `perch new` process. Because the extension starts sessions inside a VS Code terminal, that is the terminal's environment, so `TERM`, `COLORTERM`, `LANG` and `terminal.integrated.env.*` are right. Perch adds:

- `PERCH_SESSION_ID` and `PERCH_SESSION_DIR`.
- Any `--env NAME=VALUE` given to `perch new`.

**Placeholders.** In the command and in `--env` values, `{dir}` expands to the session directory and `{id}` to the session ID. `{{` and `}}` are literal braces. The directory exists, mode 0700, before the program starts. The brief proposed named `{ipc}` endpoints; one directory placeholder covers that and more. Raven starts its console with `--ipc {dir}/ipc` and keeps its own socket protocol.

### Linked variables

After a reconnect, variables in a long-lived session can point at things that no longer exist, such as the old SSH agent socket or the old VS Code window's IPC socket (which the `code` command uses).

For variables whose value is a socket path, Perch gives the program a stable path and keeps it pointing at the current client's socket:

- At `perch new`, each listed variable that is set gets a symlink, `$PERCH_SESSION_DIR/env/<NAME>`, pointing at the real value. The program sees the symlink's path as the variable's value. Variables that are not set at creation stay unset. Perch cannot add a variable to a running process.
- On each attach, the client sends its own values. The supervisor re-points each link atomically (new symlink, then `rename`). A variable the new client lacks gets a link to a path that does not exist, so programs fail as they would have with the stale value.
- Detaching leaves the links alone. Takeover re-points them to the new client.
- `connect()` follows symlinks, so `ssh`, `ssh-add` and `code` inside the session reach the agent and window of whoever is attached now.

The default list is `SSH_AUTH_SOCK` and `VSCODE_IPC_HOOK_CLI`. Each stays in the default only if the plan's tests show it working through a link on local and Remote-SSH hosts. `VSCODE_GIT_IPC_HANDLE` is left out. Git's askpass helper in VS Code also depends on `GIT_ASKPASS`, a path inside the VS Code server's versioned directory, which a link cannot fix. `perch new --link-env` and the setting `perch.linkedEnvironment` change the list. This changes the starting environment, not anything the program runs.

Other stale variables (`DISPLAY`, `SSH_CONNECTION`) are out of scope for v1. A later `perch env` could print current values for a prompt hook, in the style of tmux's `update-environment` and zmx's `print-env`.

### Shell integration

VS Code injects its shell integration only into shells it starts. A Perch terminal's process is `perch attach`, so VS Code injects nothing. Users can install it in their shell rc file with VS Code's documented manual install, guarded by `TERM_PROGRAM=vscode`. The sequences pass through Perch to VS Code while attached. They never appear in restored history, which is rebuilt from cells. Whether command detection recovers after a reattach mid-command is to be tested.

## Program exit and ending sessions

**Exit while attached.** The supervisor drains the remaining output, sends an `exited` frame with the exit code or signal, removes the session directory and exits. The client prints the status and exits with the program's code.

**Exit while detached.** The user did not see it happen. This matters on HPC, where the out-of-memory killer may end R overnight. The supervisor writes the last screen and the last 1,000 lines of history as text with SGR codes to `tail.txt`, records `state: "exited"` and the status in `meta.json`, releases the lock and exits. `list --all` shows it as exited (for example, "killed by signal 9"). Opening it shows the tail and the status. Dismissing it removes it. Exited and lost entries older than 7 days are removed by `list`.

**Kill.** The scope of a kill is the program's terminal session: every process whose session ID is the program's PID. That includes a shell's background and stopped jobs, which live in other process groups of the same session. Perch finds them by scanning `/proc` on Linux and with `proc_listallpids` and `getsid` on macOS.

1. Close the PTY master and send SIGHUP, then SIGCONT, to every process in the session. That is what closing a terminal window did before Perch, so programs behave as users expect. SIGCONT wakes stopped jobs so they can act on the SIGHUP.
2. After 5 seconds, send SIGKILL to anything left in the session.

`--force` skips to SIGKILL. SIGTERM to the supervisor is treated as `kill`. Processes that left the session with their own `setsid()` (daemons, `nohup setsid ...`) are outside the scope, and the docs say so.

## Status and resource reporting

`list` shows, per session: name, kind, working directory, age, memory, attached or not, and time of last output.

- **Memory** is the summed resident set size of the processes in the kill scope, from `/proc/<pid>/statm` on Linux and `proc_pid_rusage` on macOS. Shared pages are counted once per process, so it is an estimate, and the UI says "about". It covers the same processes that kill ends.
- **Working directory** is the program's current directory when Perch can read it (`/proc/<pid>/cwd`, `proc_pidinfo` with `PROC_PIDVNODEPATHINFO`), else the last OSC 7, else the starting directory.
- **Foreground process.** `tcgetpgrp` on the PTY master gives the foreground process group. A persistent shell then shows "running make" rather than "idle".
- **Busy or idle.** Raven's console reports this over its own socket. A Stata or shell PTY has no equivalent, so Perch shows the time of last output, as the brief proposed.

## Command-line interface

```text
perch new [options] -- <command> [args...]   start a session (attaches unless -d)
    --id ID  --name NAME  --kind KIND  --owner EXT  --annotate K=V
    --cwd DIR  --env NAME=VALUE  --link-env NAME  --label LABEL
    --scrollback LINES (default 1000, at most 10000)  -d, --detached
    --detach-key KEY|none  --restore full|screen    used when it re-attaches
perch attach <session> [--label LABEL] [--if-detached] [--detach-key KEY|none]
    [--restore full|screen] [--scrollback LINES]
                                              default --restore screen; exit 75 if
                                              --if-detached finds a live client
perch list [--json] [--all]                   --all includes exited and lost sessions
perch info <session> [--json]
perch send <session> [--text TEXT | --stdin] [--paste] [--submit]
perch kill <session> [--force] [--pid]
perch dismiss <session>                       remove an exited or lost session
perch rename <session> <name>
perch annotate <session> K=V... [--unset K]
perch detach [<session>]                      default: $PERCH_SESSION_ID
perch doctor [--json]
perch version [--json]
```

`--json` output carries `"schema": 1` and follows the additive-fields rule.

## VS Code extension

**Where it runs.** `extensionKind: ["workspace"]`, so in a remote window it runs on the remote host, next to the binary and the sessions. In a local window on macOS or Linux it runs locally. In a local window on Windows it reports that Perch needs a macOS or Linux host, and its API reports `available: false`.

**Packaging.** Platform-specific VSIXs for `darwin-x64`, `darwin-arm64`, `linux-x64`, `linux-arm64`, `alpine-x64` and `alpine-arm64`. The Linux binaries are static musl builds, so they run on HPC distributions with old glibc. A universal VSIX without a binary covers every other platform, so a Windows user's local install still works as the front end of a remote window. In that case VS Code installs the workspace extension on the remote host, using the remote's platform. Both the Marketplace and Open VSX support target platforms (`vsce package --target`, `ovsx publish --target`). The plan tests installation for local macOS, local Linux, Windows to Remote-SSH Linux and macOS to Remote-SSH Linux, from both registries. The extension uses only stable VS Code API, so it works on VS Code, VSCodium and other builds from the open-source code.

**Binary.** The extension makes sure its bundled binary is executable (mode 0755) on activation, in case a registry's archive dropped the bit. A supervisor keeps running after an update deletes the extension's directory, because the kernel keeps a running program's file alive.

Users need `perch` inside sessions (`perch detach`) and over plain SSH (`perch attach` from a phone). The bundled path changes with every update, so on activation the extension copies the binary to a stable place, `<globalStorage>/bin/<os>-<arch>/perch`, by writing a temporary file and renaming it. Global storage survives updates. The `<os>-<arch>` part keeps a home directory shared between x64 and arm64 hosts working. VS Code windows running different versions of the extension can be open at once during an update, so the copy is replaced only under an exclusive `flock` on a lock file in that directory, and only by a newer version, compared under the lock. Where `flock` fails, as on some NFS home directories, the extension still writes a temporary file and renames it, so the copy is never torn. Two windows may then each install their version, and the older one can win until the next activation of the newer one. The extension itself always runs its bundled binary, never the copy. It appends that directory to `PATH` in new terminals with `context.environmentVariableCollection`, which is stable VS Code API and writes nothing to the user's rc files. Appending means a `perch` the user installed themselves still wins. It also sets `PERCH_BIN` to the copy. The `PATH` change applies to every new terminal in the editor, not only Perch's, and the setting `perch.addToPath` (default on) and the docs say so. Users who want `perch` in plain SSH sessions add the directory, which "Perch: Show Command-Line Path" prints, to their own `PATH`.

**Session list.** A "Perch" view lists every session on the host for the current user, including ones started from the CLI, whatever workspace they came from. The view's title shows the host name, so a user who lands on a different login node behind a load-balanced alias sees why their sessions are missing. Each item shows the name, a kind icon, and a description such as `R · ~/thesis · 3h · about 4.1 GB · detached`. Inline actions open and kill. Exited and lost sessions show their status, with "Show last output" and "Dismiss". The list refreshes on activation, when the window gains focus, when the view becomes visible, after the extension's own actions, and every 5 seconds while the view is visible. While the view is hidden it still refreshes every 30 seconds, which is how `onDidEndSession` and the status bar count notice a session that ended overnight. A session that exits while attached leaves no tombstone. A window that sees a session vanish without one reports it as ended with its exit status unknown, unless that window's own terminal received the exit. A status bar item shows the number of detached sessions and opens a quick pick.

**Open.** Opening a session creates a terminal running `perch attach <id> --label <label> --restore full --detach-key none`, named after the session. If another client is attached, the extension asks first: "R: thesis is open in another window. Move it here?" Opening never changes the workspace.

**Kill** asks for confirmation in a modal that shows name, kind, working directory, age and memory, as the brief requires.

**Closing a terminal** detaches. The first time, the extension shows a notification that the session is still running, with "Kill" and "Don't show again".

**Persistent shell profile.** The extension contributes a "Persistent shell" terminal profile. It starts `perch new --kind shell -- <shell> [args]`, where the shell comes from the setting `perch.shell` (default: `$SHELL`, with `-l` on macOS to match VS Code's default there). Resolving the user's default VS Code terminal profile is not possible through stable API for every profile type, so v1 does not try.

### Creating a session from the extension

Every session the extension creates starts inside a VS Code terminal, so the program gets the terminal's real environment. The steps, which `createSession` in the API also uses:

1. Generate the session ID in the extension.
2. Build the launch: `shellPath` is the binary, `shellArgs` is `["new", "--id", id, "--label", label, "--scrollback", n, "--detach-key", "none", "--restore", "full", ..., "--", ...command]`, plus `env`.
3. `createTerminal` with that launch and `isTransient: true`.
4. Wait until `perch info <id>` reports `state: "running"`, which means `exec` succeeded, with a 10-second limit. Fail if the terminal closes first, using its `exitStatus`, if the session reports an exec error, or if the limit passes. A terminal closed before the attach handshake leaves no session behind, because the supervisor gives up after 30 seconds.

`prepareLaunch` in the API returns steps 1 and 2 without creating a terminal, so an extension's own `TerminalProfileProvider` can return a `TerminalProfile`. Raven's profile-picker path needs this. Perch watches `onDidOpenTerminal` and recognizes its terminals from `creationOptions`: a `shellPath` whose file name is `perch` (the full path may point at an extension directory an update has since deleted) and `shellArgs` of the form `new --id <id> ...` or `attach <id> ...`. Matching by creation order, as Raven's `pending_profile_sockets` queue does today, is not needed.

### Restoring terminals

Perch terminals are created with `isTransient: true`, so VS Code does not revive them itself. That avoids duplicates and terminals revived with stale environments. Instead:

- **On activation, adopt first.** Any terminal in `vscode.window.terminals` that Perch recognizes from its `creationOptions` is adopted as-is. After an extension host restart, those terminals and their attach processes are still alive.
- **Then restore.** In a window with a workspace, the extension keeps the IDs of sessions attached there in `workspaceState`. For each one still running and not already adopted, it checks `perch info` first, without opening a terminal. If the session is detached, it opens a terminal running `perch attach --if-detached --restore full`. The flag guards the race with another window: if a client got there first, the attach exits with code 75, the extension checks `perch info` to confirm the session is still running and attached elsewhere (a program can exit with 75 too), closes that terminal quietly, and keeps the session in the restore set. If the session is attached by a client with this workspace's key, that may be the previous incarnation of this window still exiting during a reload. Pending restores are retried on each list refresh for 60 seconds after activation, and then dropped for this activation. A session attached from another workspace is left where it is. Restore never takes a session from a live client. Empty windows have no workspace state, so they restore nothing. The setting `perch.restoreTerminals` (default on) turns restoring off.
- **What counts as closed.** When a Perch terminal closes, `exitStatus.reason` says why. `User`, and `Process` with any code but 75, remove the session from the restore set. `Shutdown` keeps it.

### API for other extensions

The extension's `exports` is `{ getApi(version: 1, callerId: string): PerchApiV1 | undefined }`, where `callerId` is the calling extension's ID. Asking for an unsupported version returns `undefined`, and callers fall back to their behavior without Perch. Raven and Sight find Perch with `vscode.extensions.getExtension('jbearak.perch')` and track installs and removals with `extensions.onDidChange`.

```ts
interface SessionInfo {
  id: string; name: string; kind: string; owner?: string;
  annotations: Record<string, string>;
  dir: string;                         // the session directory ({dir})
  degraded: string[];                  // e.g. 'directory-recreated'
  cwd: string; host: string; createdAt: string;
  state: 'starting' | 'running' | 'exited' | 'lost' | 'unresponsive';
  attached: boolean; attachedHere: boolean;
  lastOutputAt?: string; memoryBytes?: number;
  exit?: { code?: number; signal?: string };
}

interface PerchApiV1 {
  readonly version: 1;
  /** False on Windows hosts or when the binary cannot run. */
  available(): Promise<boolean>;
  /** The launch for a new session, without creating a terminal. */
  prepareLaunch(options: CreateOptions): { id: string; options: vscode.TerminalOptions };
  /** Creates the terminal and resolves once the program is running. */
  createSession(options: CreateOptions & { show?: boolean }): Promise<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Resolves once a terminal from prepareLaunch has a running session. */
  waitForSession(id: string, terminal: vscode.Terminal): Promise<SessionInfo>;
  listSessions(filter?: { owner?: string; kind?: string }): Promise<SessionInfo[]>;
  /** Perch terminals open in this window, with their sessions. Call on activation. */
  terminals(): ReadonlyArray<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Opens (or shows) a terminal attached to the session. */
  attach(id: string, options?: { show?: boolean }): Promise<vscode.Terminal>;
  sessionForTerminal(terminal: vscode.Terminal): SessionInfo | undefined;
  send(id: string, text: string, options?: { paste?: boolean; submit?: boolean }): Promise<'queued'>;
  kill(id: string, options?: { force?: boolean }): Promise<void>;
  annotate(id: string, set: Record<string, string>, unset?: string[]): Promise<void>;
  readonly onDidChangeSessions: vscode.Event<void>;
  /** Fires whenever Perch attaches a terminal to a session: create, open, restore, adopt. */
  readonly onDidAttach: vscode.Event<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Fires once when Perch sees that a session has ended: exited, killed or lost. */
  readonly onDidEndSession: vscode.Event<SessionInfo>;
}

interface CreateOptions {
  name: string;
  kind: string;                        // 'r', 'stata', 'shell', ...
  command: string[];                   // {dir} and {id} expand
  cwd?: string;
  env?: Record<string, string>;        // {dir} and {id} expand
  annotations?: Record<string, string>;
  terminal?: { location?: vscode.TerminalLocation; iconPath?: vscode.IconPath; color?: vscode.ThemeColor };
}
```

`send` rejects with an error whose `code` is `queue_full`, `outcome_unknown`, `no_session` or `unavailable`. `outcome_unknown` means the request may have been queued, and callers must not resend it.

Perch may activate after or before an integrating extension, and events fired before a subscriber exists are lost. So adoption is idempotent and does not depend on event order. An integrating extension calls `terminals()` and `listSessions({ owner })` when it activates, and subscribes to `onDidAttach` for later changes.

Perch records `callerId` as the `owner` of sessions created through that API object. VS Code does not tell an extension who called its exports, so a caller can lie about its ID. That is acceptable. Ownership is a convenience for finding one's own sessions, not a security boundary, and every extension already runs with the user's full privileges.

## Integrations

These changes belong in Raven and Sight, not in Perch. They are listed so that Perch's API covers them. Each project will design its side in its own repository.

**Sight (Stata).**

- Start Stata with `createSession({ kind: 'stata', command: [stataCli], ... })` when Perch is available, and keep the current terminal path otherwise.
- Replace `terminal.sendText('do "<file>"')` with `send(id, 'do "<file>"', { submit: true })`. Sends then work while the terminal is closed or attached in another window.
- On activation, adopt surviving Stata sessions through `terminals()` and `listSessions({ owner: 'jbearak.sight' })`, so the "current Stata terminal" survives a window reopen.
- Stata pauses at `--more--` when output fills the screen, and a detached Stata waits there forever, because Perch never answers it. Sight's docs should tell users to put `set more off` in `profile.do`.

**Raven (R).** Raven's console keeps its own socket protocol and its `queued`, `done` and `error` replies. What Raven needs to change:

- **Launch.** Start the console with `command: [raven, 'console', '--ipc', '{dir}/ipc', ...]` and `annotations: { 'raven.console': 'ipc' }`. For the profile picker, use `prepareLaunch`, and map terminals to sessions with `sessionForTerminal`.
- **Sending to a detached session.** Raven picks its target R session from `listSessions({ owner: 'jbearak.raven' })`, not only from open terminals, and connects to `<dir>/ipc` directly. A session with no terminal still receives code. Raven's fallback route, which types code into the terminal today with `terminal.sendText`, uses Perch's `send` for Perch sessions, because `sendText` reaches nothing while detached. If `degraded` includes `directory-recreated`, the console socket is gone for good and the `send` route is the only one left.
- **Descriptor files.** Raven keeps two files in the session directory, each replaced atomically. R's side and the editor's side never write the same file. The editor writes `raven-window.json` (plot server port and token, data viewer directory), and the window that writes it is the one that receives plots and data viewers. Only two events make a window write it: Perch attaches a terminal in that window (`onDidAttach`, including restore and adoption of a terminal there), or the user sends code from that window to a detached session. A window that merely lists a session never writes it. If two windows race, the later write wins, and that is the window the user acted in last. R's bootstrap profile writes `raven-r.json` (R session ID, httpgd URL and token) whenever the device starts. Both are mode 0600. The bootstrap profile reads the port and token from `raven-window.json` at every request, falling back to the environment. Secrets stay out of annotations.
- **Adoption.** On activation, Raven calls `terminals()` and `listSessions`. For each Raven terminal in this window, and on every later `onDidAttach`, it writes `raven-window.json`, registers the console socket and reads `raven-r.json` to re-register the httpgd device. Detached sessions found by `listSessions` are only recorded as possible targets. Adoption is idempotent. No code is sent to R.
- **Detached plots.** While no window is connected, R's posts to the plot server fail. Because R records the httpgd URL in `raven-r.json` whenever the device starts, a window that adopts the session later finds plots made while detached.
- **Cleanup.** Raven currently deletes the console socket's directory (`fs.rm(dirname(socket), { recursive: true })`) and calls `markSessionEnded` when an R terminal closes. For a Perch-backed terminal, closing means detaching. Raven must skip both and clean up on `onDidEndSession`.

## Platforms

| Host | Status |
|------|--------|
| Linux x64, arm64 (glibc or musl) | Supported. Static musl binary. |
| macOS x64, arm64 | Supported. macOS 11 or later. |
| Windows | Front end only, through Remote-SSH, WSL or Dev Containers to a supported host. |

## Security

- Directory and socket permissions as above, `O_NOFOLLOW` path handling inside the runtime directory, and a peer uid check on every connection.
- No network listeners.
- `meta.json` holds the command line and working directory, readable only by the user. Annotations are readable by every extension through the API; secrets go in owner files.
- `send` and attach input go only to the session's PTY. The protocol has no operation that runs a command on the host.

## Known risks

From the brief, with where this design handles each:

| Risk | Handling |
|------|----------|
| Stale environment | Linked socket variables for the agent and the `code` command; other variables deferred |
| Shell integration | Manual install works through the relay; history never replays its marks; recovery after reattach to be tested |
| Terminal queries while detached | Supervisor answers DSR and DA from its model, with defined reply ownership |
| Cleanup | List with memory, kill with confirmation, status bar count, tombstones for exited and lost sessions |
| HPC login nodes | Documented; `perch doctor` reports what it can detect |
| Reboots | Documented. Nothing survives. |
| Busy or idle | Time of last output and foreground process |
| Stata licenses | Documented: a detached Stata may hold a license seat |

New risks this design adds:

- **avt is pre-1.0 and changes fast.** Perch pins an exact version and keeps the emulator behind its interface. The fidelity tests catch regressions on upgrade.
- **Memory for history.** History costs about 17 bytes per cell: about 2 MB per session at the defaults, about 85 MB in the worst case (10,000 lines at 500 columns).
- **Restore fidelity is bounded by the contract above.** Programs using kitty graphics or other state outside the tracked set may not come back exactly.
- **Host policy can end sessions.** Documented, detected by `perch doctor` where possible, and not fixable by Perch in v1.

## What the plan must verify

Some claims depend on behavior that only a test can settle. The plan's first milestone covers the riskiest ones.

- The fidelity contract and every transition in [Terminal queries](#terminal-queries) and [Flow control](#flow-control), in a headless xterm.js and in tmux: split UTF-8, invalid bytes, partial and unterminated control strings, RIS and DECSTR, kitty push and pop on both screens, queries split across reads, keystrokes interleaved with delayed replies, takeover during a restore, stalls during live output and during a restore, and attaches at different widths.
- That vte and avt agree on sequence boundaries (CAN, SUB, ESC inside strings, C1 bytes inside UTF-8), with a differential test.
- How xterm.js, Terminal.app, iTerm and tmux handle DECSTR, `ESC [3J` and `ESC [?1049l` when already on the primary screen.
- Raven's console (reedline) at a prompt while detached, and behind a large queued send.
- Linked variables through a symlink, for each default variable, locally and over Remote-SSH.
- Survival across window close, SSH drop, VS Code server restart and update, and logout with `KillUserProcesses` on and off.
- `flock` on directories and the systemd-tmpfiles skip, on Linux.
- `creationOptions.shellArgs` on terminals created before the current extension host, and on terminals from a profile provider.
- Installation from both registries for each local and remote combination, and that the darwin binaries need no more than an ad-hoc signature.
- That `PERCH_SESSION_ID` inherited by a process that left the session is checked with `getsid` before Perch trusts it.
- That `ESC [?1049l` on the primary screen is harmless in each terminal tested.
- That keystrokes and resizes reach the program while `perch attach` is under backpressure.
- Memory after widening a session with deep history.
- A terminal's reply to a forwarded query while a large `send` is queued.
- Two concurrent binary-copy updates, with and without `flock`.

## Deferred from v1

- Transient systemd scopes for supervisors.
- `perch env` for non-socket stale variables.
- History beyond 10,000 lines, for example a compact text store for lines avt has evicted.
- Multiple simultaneous clients.
- Listing sessions on hosts the window is not connected to.
- Resolving VS Code's default terminal profile for the persistent shell.

## Repository layout

```text
Cargo.toml                 workspace
crates/perch/              the binary (supervisor, client, CLI)
editors/vscode/            the extension
docs/                      design, plan, user docs
```

This mirrors Raven's layout.

## Decisions on the brief's open questions

- **License identifier.** `GPL-3.0-only`, to match Raven, which declares `GPL-3.0` (the deprecated SPDX form of GPL-3.0-only).
- **Remote hosts the window is not connected to.** Not listed in v1, as the brief recommended. The extension runs on one host and sees that host.
- **Names and layout.** As above.

## Review history

### Revision 5

From the fourth review (Sonnet agreed, Terra did not):

- A sequence left unfinished for a second now belongs to the supervisor. The client never sees any of it, so it cannot straddle ownership, collide with restored modes, or print stray text. This replaces the C1 rewriting and the partial-introducer handling.
- Restores are delivered ahead of live output under backpressure and the stall rule, with a stated size bound, instead of being squeezed into the live queue.
- A terminal's reply to a forwarded query jumps ahead of queued sends, like the supervisor's own replies.
- `perch new` takes `--detach-key` and `--restore`. Re-attaching in the same process reuses the restore mode.
- The screen is clamped to 500 by 500, and history memory is stated honestly, including its worst case.
- The prologue also resets newline mode, G1 and shift-out, tab stops, the saved cursor and modifyOtherKeys. The primary screen's kitty stack is restored before the dump.
- Defined the `Attach` frame. The stall timer runs only while frames are pending. A session that vanishes without a tombstone counts as ended. The binary copy has a fallback without `flock`.

### Revision 4

From the third review by Sonnet and Terra:

- Flow control now applies backpressure to a slow client and drops output only after a 5-second stall, and then only whole unsent frames. `too_slow` counts stalls. `perch attach` never buffers more than a frame.
- Restores are taken at a parser ground state, so avt's dump has no tail to duplicate and no query straddles the change of ownership. The 1-second fallback, C1 rewriting and control-string swallowing are specified.
- Three restore modes. The CLI no longer erases scrollback by default, and the first attach of `perch new` restores nothing.
- The prologue resets everything Perch or avt tracks, for terminals with and without DECSTR. Modes avt tracks are set only by its dump; Perch's own modes follow it. Kitty stacks are per screen.
- Query rules spell out reading a departing client's pending input, recorded cursor positions, and the one case left unanswered.
- The program gets back the creator's umask, directory and signal mask.
- Restore checks `info` before opening a terminal, uses exit code 75 for a lost race, and retries for 60 seconds.
- Raven's window descriptor is written only by the window the user acted in.
- `PATH` is appended, the copy is replaced under a lock, and the extension runs its bundled binary. The extension turns off the detach key.
- History has a cell budget. `onDidEndSession` has a 30-second cadence while the view is hidden.
- Added the list of claims the plan must verify.

### Revision 3

From the second review by Sonnet and Terra:

- Restore now starts with a fixed prologue (`CAN`, leave the alternate screen, DECSTR) that brings any terminal to a known state, including one left mid-sequence by a dropped frame. Added a second clear before the dump, because avt's dump assumes a blank screen.
- Split restore into a full restore (attach, with history) and a resync (after lag, screen only), and made restore frames count against the queue cap. Output frames carry epochs. Repeated resyncs detach a client that cannot keep up.
- Query ownership passes when the restore is enqueued. Replies are matched by shape, not by "any input". Outstanding queries are answered on overflow, detach and takeover, and stale ones expire. Supervisor replies jump ahead of queued sends.
- An incremental UTF-8 decoder feeds avt. The model is fed up to each query before Perch answers it. avt's unfinished-sequence tail is rewritten to 7-bit. The fidelity contract is narrowed to what avt stores and is tested first.
- Cut the second history tier. History is avt's scrollback, sized from the client's scrollback setting.
- An exec-result pipe sets `running`. `starting` times out after 30 seconds. `list` locks before reading, and every deletion happens under an exclusive lock after an inode check. The supervisor checks its directory every 60 seconds and touches every entry, including other programs' files, every hour. Losing the directory is described as a real loss for owner sockets, with Raven's fallback.
- `kill --pid` checks recorded start times before signaling.
- Labels have a workspace key and a window key. Restores use `attach --if-detached` and never take a session from a live client.
- The CLI reaches `PATH` through a stable copy in global storage and `environmentVariableCollection`.
- API: `onDidEndSession`, `degraded`, and recognition of terminals by file name and arguments. Raven: sending to detached sessions, two single-writer descriptor files, and the `send` fallback.
- Frozen compatibility set now includes the core metadata and `list --json` fields.
- Dropped the claim that shpool only runs shells (`shpool attach --cmd` exists).

### Revision 2


From the first review by Sonnet and Terra:

- Screen model changed from vt100 to avt, after a spike, because restore must survive the alternate screen and narrower clients. History is rebuilt from cells in two tiers, not replayed from raw bytes, which removes the replay sanitizer and the seam between history and screen. Added a fidelity contract.
- Added flow control: the PTY is always read, client queues are bounded, overflow resynchronizes.
- Defined who answers each terminal query, including during attach, takeover and disconnect. DA1 now claims only a VT100.
- The lock is now on the session directory itself, which also protects it from systemd-tmpfiles. Sessions are published by build-then-rename. `list` no longer deletes live or starting sessions, and lost sessions become tombstones.
- Kill and memory now cover the program's whole terminal session, including a shell's background and stopped jobs.
- The program is forked after the creator attaches, with the creator's size and termios. Descriptor hygiene is specified. Model panics are isolated.
- Sends have a bounded queue, `queue_full` and `outcome_unknown`. Dropped the canonical-mode length check, which could not be made correct.
- Specified session creation from the extension (`prepareLaunch`, readiness, `creationOptions` mapping), adoption after an extension host restart, silent takeover from the same workspace, and exit reasons.
- Expanded what Raven must change: descriptor file, idempotent adoption, detached plots, and cleanup on terminal close. Added Stata's `--more--` note for Sight.
- Linked variables are defined for absent, missing and takeover cases. Dropped `VSCODE_GIT_IPC_HANDLE`. Defaults depend on tests.
- Process lifetime is described as conditional on host policy, with service cgroups added. The systemd scope moved to deferred.
- Protocol: `capabilities` in `Welcome`, a frozen core, a 12-month window for any bump, timeouts, and signal-based recovery.
- Cut from v1: the file watcher, default profile resolution, `perch env`, `PATH` install, and the systemd scope.
