# Perch design

Status: draft for review. This document turns [brief.md](brief.md) into decisions. Each section says what Perch does and why, and names the alternatives it rejected.

## What Perch is

Perch has two parts.

- `perch`, a small Rust binary for macOS and Linux. It starts a session, keeps it alive with no client attached, and lets clients list, attach to, send to and kill sessions.
- A VS Code extension (`jbearak.perch`) that bundles the binary, shows the session list, opens sessions in terminals, provides a "Persistent shell" terminal profile, and gives Raven and Sight a versioned API.

A session is one program (R, Stata, a shell, anything) running on a pseudo-terminal that Perch owns. Closing the editor window, losing the SSH connection or restarting the VS Code server does not end it. Rebooting the host, or killing the supervisor process, does.

Perch never writes anything into a session on its own. Bytes reach a session's input only from an attached terminal, from an explicit `send` by the user or an integrating extension, or as a reply to a terminal query the program itself sent while no terminal was attached (see [Terminal queries while detached](#terminal-queries-while-detached)). A real terminal would send the same reply.

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

- **Passthrough while attached, like dtach and abduco.** Live output goes to the client byte for byte. VS Code's terminal keeps its own scrollback, selection, links and shell integration sequences, because nothing re-renders them. tmux and screen redraw everything through their own emulator. That costs native scrollback, adds a prefix key, sets `TERM` to `tmux-256color` or `screen`, and drops escape sequences tmux does not know about unless the user enables passthrough. Those costs are the reason Raven's docs today describe tmux as a workaround and not a solution.
- **A screen model on the side, like shpool and zmx.** dtach and abduco keep nothing, so a reattached terminal shows a blank screen until the program redraws, and full-screen programs and line editors lose their mode state (bracketed paste, application cursor keys, mouse reporting). Perch feeds all output through a terminal emulator that it uses only to restore the screen on attach and to answer queries while detached. It never renders live output from it.
- **One supervisor per session, like dtach, abduco and zmx, and unlike tmux and shpool.** See [Process model](#process-model).
- **Answers cursor-position queries while detached.** zmx answers DA1 and DA2 so that fish does not wait 10 seconds at a prompt. Raven's console uses reedline, which asks for the cursor position (DSR 6) every time it draws a prompt and waits up to 2 seconds for the reply. A detached Raven console that receives code over its socket would stall or fail at every prompt. Perch answers DSR 5, DSR 6, DA1 and DA2 from its screen model when no terminal is attached.
- **No code injection.** zmx's `write` command types a `printf ... | base64 -d > file` pipeline into the shell. tmux setups commonly run `send-keys` hooks. Perch has no feature that composes commands for the program.
- **A contract with clients that outlive versions.** Supervisors run for weeks. The extension that started them gets updated and its directory, binary included, is deleted. Perch versions its wire protocol and its metadata, and every client release must speak every protocol version a still-running supervisor might use. tmux has the same problem and refuses to talk to a server of a different protocol version ("server version is too old"), which strands the session until the user finds a matching binary. zmx is pre-1.0 and makes no promise.
- **Not a network layer.** mosh and Eternal Terminal solve roaming and lossy links for one login. VS Code Remote already owns the transport. Perch sits on the host and does not care how the editor reaches it.
- **Longer-lived than VS Code's pty host.** VS Code keeps terminals across a window reload and short disconnects, but not past the reconnection grace period, a server restart or a server update. Perch sessions survive all three.

### Why not build on an existing tool

- **tmux as a backend** (`tmux -L perch`, control mode). tmux is missing on many macOS machines and HPC nodes, versions vary widely, and the re-rendering costs above apply. A per-version compatibility layer would be needed anyway.
- **zmx as a backend.** It is the closest tool and is MIT-licensed. It is pre-1.0 with no cross-version guarantee, so the extension could not promise that a new release still reaches old sessions. It needs a Zig toolchain for four targets. It lacks DSR answers, machine-readable metadata (kind, owner, working directory, memory), and the placeholder and environment features below. It also ships the injection-based `write`. Perch borrows its ideas: per-session daemons, an emulator-backed restore, environment tracking. It does not borrow its code.
- **shpool as a backend.** It needs a long-lived daemon that is either a systemd user service (not available on macOS or many HPC systems) or autodaemonized. Upgrading that daemon means restarting it, which kills every session or blocks the upgrade. It only runs shells.

## Process model

**Decision: one supervisor process per session, no central server.** The brief proposed this, and it holds up.

- A crash or memory leak in one supervisor affects one session.
- There is nothing to install, start or upgrade. Old and new supervisors coexist, each running the binary that started it.
- `list` is a directory scan of a few small files. With tens of sessions this costs milliseconds.

The cost is that cross-session features (one socket to watch every session, server-wide options) are harder. Perch needs none.

`perch new` forks. The child calls `setsid()`, forks again so it cannot reacquire a controlling terminal, ignores SIGHUP and SIGPIPE, points stdin and stdout at `/dev/null`, sends stderr to a small log file in the session directory, and becomes the supervisor. The supervisor reports readiness, or the reason it failed, to the original process over a pipe before that process exits. So `perch new` returns only once the socket is listening.

After the double fork the supervisor is a child of init (or of a subreaper). VS Code, its pty host and sshd do not find it when they kill their process trees or send SIGHUP to their sessions.

The supervisor opens a PTY pair, forks the program with the slave as its controlling terminal in a new session, and then runs one single-threaded `poll()` loop over the listening socket, the PTY master, client connections and a self-pipe for SIGCHLD.

### What can still kill a session

- **systemd `KillUserProcesses=yes`.** When a user's last login session ends, logind kills every process in the session's scope, `setsid` or not. Most distributions ship `no`, but some HPC sites turn it on. When `systemd-run --user` is available and the user's systemd manager is lingering, `perch new` can start the supervisor in its own transient scope (`perch new --systemd-scope=auto`, on by default). Otherwise Perch warns once, in `perch doctor` and in the extension, that sessions will not survive logout here. Whether to attempt the scope at all when linger is off is an open question for the plan.
- **HPC login-node reapers** that kill long-lived or high-memory processes. Perch cannot stop these, and the docs say so.
- **Containers.** A Dev Container that stops when its window closes takes its sessions with it.
- **Flatpak and Snap editors running locally.** The sandbox may not let processes outlive the editor. Remote hosts are unaffected.
- **Reboots**, and anything that kills the supervisor.

## Files and sockets

**Decision: everything lives under one short, node-local runtime directory, `/tmp/perch-<uid>/`.** This rejects the brief's `~/.local/state/perch/`, for three reasons.

1. **Shared home directories.** On HPC clusters `$HOME` is shared across login nodes over NFS. A state directory there would show node A's sessions on node B, where every one of them looks dead and `list` would delete them. Sessions are node-local, so their records must be node-local too.
2. **Socket path length.** `sun_path` is 104 bytes on macOS and 108 on Linux. `/tmp/perch-1000/k3x9q2ab/sock` is 31 bytes. Home directories on clusters can be long enough that `~/.local/state/perch/<id>/sock` breaks.
3. **Not `$XDG_RUNTIME_DIR`.** logind removes `/run/user/<uid>` when the user's last login session ends. That is exactly when a Perch session needs to survive. tmux uses `/tmp` for the same reason.

`$PERCH_RUNTIME_DIR` overrides the location.

Layout:

```text
/tmp/perch-<uid>/              0700, owned by the user
  <id>/                        0700, one per session
    sock                       0600, the supervisor's socket
    lock                       flock held by the supervisor for its lifetime
    meta.json                  0600, metadata (see below)
    supervisor.log             0600, capped at 256 KiB
    env/                       linked environment (see below)
      SSH_AUTH_SOCK -> ...
    tail                       written only when the program exits while detached
```

On startup, and before trusting the directory, every Perch process checks with `lstat` that `/tmp/perch-<uid>` is a real directory (not a symlink) owned by the current uid with mode 0700, as tmux does for its socket directory. If another user created it first, Perch refuses and tells the user to set `PERCH_RUNTIME_DIR`. The supervisor also checks the peer's uid on every connection (`SO_PEERCRED` on Linux, `getpeereid` on macOS).

**Liveness.** The supervisor holds an exclusive `flock` on `lock` for as long as it runs. `list` tries a non-blocking `flock`. If it gets the lock, the supervisor is gone. This survives PID reuse and does not depend on the socket. A session directory whose lock is free and whose `meta.json` says the program was running is reported once as `lost` and then removed.

**Temp cleaners.** systemd-tmpfiles ages out `/tmp` entries after 10 days on many distributions, and macOS's periodic job uses 3 days. Every hour, the supervisor touches its files and checks that `sock` still exists and is its own inode. If not, it recreates the directory, socket and metadata.

**Session IDs** are 8 random characters from `[a-z0-9]`. Each session also has a display name, set at creation and renamable later. The CLI accepts an ID, a unique ID prefix, or an exact name.

### Metadata

`meta.json` is written atomically (temp file plus rename). Unknown fields must be ignored.

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
  "pid": 41201,
  "perchVersion": "0.1.0",
  "protocol": { "min": 1, "max": 1 },
  "state": "running",
  "exit": null
}
```

`kind` is free text. The extension knows `r`, `stata` and `shell` and shows anything else as a generic program. `owner` is the extension ID that created the session, or absent. `annotations` is a string map that owners use to find their own wiring (for example, which file in the session directory is the console socket). Perch does not interpret it.

Fields that change often (attached or not, last output time, memory) are not stored. `list` asks the supervisor for them.

## Wire protocol

Clients reach the supervisor over its Unix socket. Each frame is a 4-byte big-endian length, a 1-byte type and a payload. The length covers type plus payload and is capped at 1 MiB.

The first frame is always `Hello` from the client, with a JSON payload `{"min":1,"max":1,"client":"perch 0.1.0","mode":"control"|"attach"}`. The supervisor picks the highest version both sides speak and answers `Welcome` with `{"protocol":1,"perch":"0.1.0"}`. If there is no overlap it answers `Error` with both ranges, and the client tells the user which Perch version started the session.

Control frames carry JSON requests and responses, `{"id":7,"op":"info"}` and so on. Unknown fields are ignored and unknown ops get an `unsupported` error, so new features can be added without a new protocol version. A protocol version bump is reserved for changes old peers would misread.

Attach frames carry raw bytes (input, output) or small JSON (resize, detach, exit).

| Op | Request | Response |
|----|---------|----------|
| `info` | | live fields: `attached`, `clientPid`, `lastOutputAt`, `lastInputAt`, `rssBytes`, `foreground` (pid and name of the PTY's foreground process group), `rows`, `cols`, `state`, `exit` |
| `send` | `data` (UTF-8 string) or `dataBase64`; `paste` (bool); `submit` (bool) | `queued` once the bytes are in the supervisor's input queue |
| `kill` | `force` (bool) | `ok`, then the supervisor exits |
| `rename` | `name` | `ok` |
| `annotate` | `set` map, `unset` list | `ok` |
| `detach` | | `ok`; the attached client is told it was detached |

**Compatibility rule.** A Perch release must speak every protocol version that any earlier release with a still-supported extension could have started. In practice protocol 1 stays forever, and a protocol 2 would be added alongside it.

## Sending input

`send` exists so that editors can send code while no terminal is attached, and so that sends from Sight do not depend on a terminal being focused.

- The supervisor keeps one FIFO input queue per session. Keystrokes from the attached client and `send` requests share it. A send's bytes are never interleaved with typed bytes.
- `queued` is the acknowledgement. Once a client has it, the send will be written in order, unless the session ends first. As with Raven's console socket, a client must never resend after `queued`.
- `submit: true` appends a carriage return, which is what pressing Enter sends.
- `paste: true` wraps the data in bracketed-paste markers (`ESC [200~` and `ESC [201~`) only if the program has turned bracketed paste on, which the screen model tracks. Otherwise the bytes go raw. The caller decides; Perch does not guess.
- When the program reads in canonical mode, the kernel caps a line at 4095 bytes on Linux. The supervisor checks `ICANON` on the PTY and rejects a send that would exceed it with `line_too_long`, instead of letting the kernel cut it silently. Sight sends `do "<file>"`, which is far below the limit.
- `perch send` writes no other bytes. It does not add `set more off`, answer `--more--`, or load anything.

## Attaching

**Decision: a terminal attaches by running `perch attach <id>` as the terminal's process.** The client puts its own terminal in raw mode and relays bytes between it and the supervisor socket. It forwards SIGWINCH as resize frames.

The alternative was a VS Code `Pseudoterminal` that speaks the protocol from the extension host. Rejected because:

- `perch attach` works from any terminal on the host, with or without VS Code. A user can SSH in from a phone and reach the same R session.
- Terminal replies (cursor position, device attributes, colors) come from the real terminal and flow back through the same pipe with no extra code.
- The protocol has one implementation, in Rust. The extension calls the CLI.

The cost is a second PTY hop, which tmux, dtach and abduco all have too.

**One client at a time; a new attach takes over.** The brief proposed this and it holds for v1. The old client receives a `detached` frame with the reason `takeover`, prints `[perch] Opened in another window. Press Enter to take it back, or close this terminal.` and waits. Multiple simultaneous clients (zmx's leader model) would need rules for whose size wins and whose keystrokes count. That is not worth it for an editor where the user is in one window at a time.

**Detach.** Closing the terminal detaches. So does `perch detach` from inside the session, and the detach key `Ctrl-\`, the dtach and abduco default. `Ctrl-\` sends SIGQUIT in a cooked terminal, which kills R with a core dump, so intercepting it removes a hazard. `--detach-key none` turns it off for users who need it.

**Nesting.** Inside a session `PERCH_SESSION_ID` is set. `perch attach` refuses to attach a session to itself.

### Restore on attach

On attach, the client sends its size and the supervisor:

1. Resizes the PTY and the screen model to the client's size. If the size changed, the kernel delivers SIGWINCH and the program redraws as it would for any resize. If the size is unchanged, Perch does not force a redraw, because the model is already exact and a forced redraw makes some line editors print a second prompt.
2. Sends the history: the main-screen byte ring, cut at a line boundary, with SGR reset at the start (see below).
3. Erases the visible screen and draws the model's visible screen, cursor position, cursor style and input modes, using vt100's `state_formatted()` plus the modes Perch tracks itself (focus reporting, synchronized output, kitty keyboard flags, title, working directory as OSC 7).
4. Switches to live passthrough.

The result is that the terminal's scrollback holds the history, rendered by the terminal at its own width, and the visible screen is exact. Full-screen programs (`vim`, `less`, `htop`) come back as they were, because the model tracks the alternate screen.

**History ring.** A byte ring (default 2 MiB) holds main-screen output. Output written while the program is in the alternate screen does not enter the ring, as in tmux, so a `vim` session in the middle of history does not replay as garbage. The ring also strips sequences that are unsafe or wrong to replay:

- queries (DSR, DA, DECRQM, XTVERSION, kitty keyboard query, OSC color queries, DCS requests). Replaying a query makes the new terminal answer it, and the answer lands in the program's input as garbage.
- OSC 52 clipboard writes. Replaying one would overwrite the user's clipboard on every attach.
- shell integration marks (OSC 633 and OSC 133). Replayed, they create stale command decorations in VS Code.

**Screen model.** Decision: the [`vt100`](https://crates.io/crates/vt100) crate (MIT), with no scrollback of its own. It is pure Rust, cross-compiles to every target with no extra toolchain, is widely used, is maintained (0.18, October 2026) and already serializes screen state. Its cells take 32 bytes, so a 50 by 200 screen costs about 320 KB. Keeping history as bytes rather than cells keeps the supervisor small: a 2 MiB byte ring holds tens of thousands of lines of R output, where the same in cells would cost tens of megabytes. A second, lightweight parse with the `vte` crate (Apache-2.0 or MIT, the parser underneath vt100) finds alternate-screen switches, queries and OSC sequences at exact byte offsets.

The brief proposed libghostty-vt. It is more complete (kitty keyboard, graphemes, a formatter built for this purpose) and zmx shows it works here. It is pre-1.0, its Rust bindings are young, and it needs Zig in the build for four targets. The emulator sits behind a small internal interface (`feed`, `resize`, `cursor`, `modes`, `serialize`), so it can be swapped in later without touching the rest.

### Terminal queries while detached

Programs ask the terminal questions by writing escape sequences and reading the reply from their input. With no terminal attached, nothing answers. The supervisor answers only when no client is attached at the moment it parses the query, and only these:

| Query | Reply |
|-------|-------|
| DSR 5 (`ESC [5n`) | `ESC [0n` |
| DSR 6 (`ESC [6n`) | `ESC [<row>;<col>R` from the model |
| DA1 (`ESC [c`) | `ESC [?62;22c` (VT220 with ANSI color) |
| DA2 (`ESC [>c`) | `ESC [>0;0;0c` |

Programs that probe for features (OSC 11 background color, kitty keyboard, XTVERSION) conventionally send DA1 last as a sentinel, so answering DA1 tells them "not supported" without Perch inventing colors. Perch does not answer color queries, because a wrong answer could flip a program's light or dark theme.

## Environment

### Inside the session

The program starts with the environment of the `perch new` process, which is normally the VS Code terminal's environment (so `TERM`, `COLORTERM`, `LANG` and `terminal.integrated.env.*` settings are right), plus:

- `PERCH_SESSION_ID` and `PERCH_SESSION_DIR`.
- Any `--env NAME=VALUE` given to `perch new`.

**Placeholders.** In the command and in `--env` values, `{dir}` expands to the session directory and `{id}` to the session ID. `{{` and `}}` are literal braces. The directory exists, mode 0700, before the program starts. The brief proposed named `{ipc}` endpoints; one directory placeholder covers that and more. Raven starts its console with `--ipc {dir}/ipc` and keeps its own socket protocol.

### Stale environment after reconnecting

After a reconnect, variables in a long-lived session can point at things that no longer exist: the old SSH agent socket, the old VS Code window's IPC socket (which the `code` command uses), the old Git credential helper socket.

**Linked variables (on by default).** For variables whose value is a socket path, Perch gives the program a stable path that it keeps pointing at the right socket. At `perch new`, for each listed variable present in the environment, the program sees `$PERCH_SESSION_DIR/env/<NAME>`, a symlink to the real value. On each attach, the client sends its own values and the supervisor atomically re-points the links. `connect()` follows symlinks, so `ssh`, `git` and `code` inside the session reach the agent, credential helper and window of whoever is attached now. Default list: `SSH_AUTH_SOCK`, `VSCODE_IPC_HOOK_CLI`, `VSCODE_GIT_IPC_HANDLE`. `--link-env` and the extension setting `perch.linkedEnvironment` change it. This changes the starting environment, not anything the program runs.

**Tracked variables (opt-in by the user).** For other variables (`DISPLAY`, `SSH_CONNECTION`, `XAUTHORITY`, ...) a path trick does not work. Each attach records the client's values in the session directory. `perch env` prints `export` and `unset` lines for the current values, and the docs show a prompt hook (`eval "$(perch env)"` in `precmd` or `PROMPT_COMMAND`) for users who want it. This follows tmux's `update-environment` and zmx's `print-env`. Perch never runs the hook itself.

### Shell integration

VS Code injects its shell integration only into shells it starts. A Perch terminal's process is `perch attach`, so VS Code injects nothing. Users can install it in their shell rc file with VS Code's documented manual install (`. "$(code --locate-shell-integration-path zsh)"`, guarded by `TERM_PROGRAM=vscode`). The sequences pass through Perch to VS Code while attached, and Perch strips them from replayed history. Whether command detection survives a reattach mid-command is to be tested.

## Program exit and ending sessions

**Exit while attached.** The supervisor drains remaining output, sends an `exited` frame with the exit code or signal, removes the session directory and exits. The client prints the status and exits with the program's code.

**Exit while detached.** The user did not see it happen. This matters on HPC, where the out-of-memory killer may end R overnight. The supervisor writes the serialized screen and the end of the history ring to `tail`, records `state: "exited"` and the status in `meta.json`, and exits. `list` shows the session as exited (for example, "killed by signal 9") until the user opens it, which shows the tail and the status, or dismisses it. Exited entries older than 7 days are removed by `list`.

**Kill.** `perch kill` closes the PTY master and sends SIGHUP to the program's process group. That is what closing a terminal window did before Perch, so programs behave as users expect. If anything in the group remains after 5 seconds, it sends SIGKILL. `--force` sends SIGKILL immediately. SIGTERM to the supervisor is treated as `kill`.

## Status and resource reporting

`list` shows, per session: name, kind, working directory, age, memory, attached or not, and time of last output. Memory is the summed resident set size of the program and its descendants, read from `/proc` on Linux and `libproc` on macOS. The working directory is the program's live `cwd` when Perch can read it, else the last OSC 7 it saw, else the starting directory. The foreground process (from `tcgetpgrp` on the PTY) lets a persistent shell show "running make" rather than "idle".

Raven's console reports busy and idle over its own socket. A Stata or shell PTY has no equivalent, so Perch shows the time of last output instead, as the brief proposed.

## Command-line interface

```text
perch new [options] -- <command> [args...]   start a session (attaches unless -d)
    --id ID  --name NAME  --kind KIND  --owner EXT  --annotate K=V
    --cwd DIR  --env NAME=VALUE  --link-env NAME  --history BYTES
    --systemd-scope auto|always|never  -d, --detached
perch attach <session> [--detach-key KEY|none]
perch list [--json] [--all]                   --all includes exited sessions
perch info <session> [--json]
perch send <session> [--text TEXT | --stdin] [--paste] [--submit]
perch kill <session> [--force]
perch dismiss <session>                       remove an exited session
perch rename <session> <name>
perch annotate <session> K=V... [--unset K]
perch detach [<session>]                      default: $PERCH_SESSION_ID
perch env [<session>]                         export lines for tracked variables
perch doctor                                  check runtime dir, systemd, limits
perch version [--json]
```

`--json` output carries `"schema": 1`. The extension and the binary it ships are released together, so this schema only needs to stay compatible with the extension in the same release. Scripts get the same additive-fields rule as the metadata.

## VS Code extension

**Where it runs.** `extensionKind: ["workspace"]`, so in a remote window it runs on the remote host, next to the binary and the sessions. In a local window on macOS or Linux it runs locally. In a local window on Windows it reports that Perch needs a macOS or Linux host, and its API reports `available: false`.

**Packaging.** Platform-specific VSIXs for `darwin-x64`, `darwin-arm64`, `linux-x64`, `linux-arm64`, `alpine-x64` and `alpine-arm64`. The Linux binaries are static musl builds, so they run on old HPC distributions with old glibc. A universal VSIX without a binary covers every other platform. All of them are published to the VS Code Marketplace and to Open VSX, which supports target platforms (`ovsx publish --target`). The plan must confirm this against Open VSX before release. The extension uses only stable VS Code API, so it works on VS Code, VSCodium and other builds from the open-source code.

**Binary path.** The extension runs the binary from its own directory. A supervisor keeps running after an update deletes that directory, because the kernel keeps the deleted file's contents alive while a process uses it. For users who want `perch` on their `PATH` inside sessions (for `perch env` and `perch detach`), the command "Perch: Install Command-Line Tool" copies the binary to `~/.local/bin/perch`. The extension refreshes that copy on later activations if it is older. It never changes `PATH` or rc files.

**Session list.** A "Perch" view lists every session on the host for the current user, including ones started from the CLI, whatever workspace they came from. Each item shows the name, a kind icon, and a description such as `R · ~/thesis · 3h · 4.1 GB · detached`. Inline actions open and kill. Exited sessions show their status, with "Show last output" and "Dismiss". The list refreshes on activation, when the window gains focus, when the view becomes visible, after the extension's own actions, on a file watcher on the runtime directory, and on a 5-second timer while the view is visible. A status bar item shows the count of detached sessions and opens a quick pick.

**Open.** Opening a session creates a terminal running `perch attach <id>`, named after the session. If the session is attached in another window, the extension asks first: "R: thesis is open in another window. Move it here?" Opening never changes the workspace.

**Kill** asks for confirmation in a modal that shows name, kind, working directory, age and memory, as the brief requires.

**Closing a terminal** detaches. The first time, the extension shows a notification that the session is still running, with "Kill" and "Don't show again".

**Persistent shell profile.** The extension contributes a "Persistent shell" terminal profile. It starts `perch new --id <generated> --kind shell -- <shell>`, using the shell and arguments of the user's default terminal profile (`terminal.integrated.defaultProfile.*` resolved against `terminal.integrated.profiles.*`), falling back to `$SHELL`.

**Restoring terminals.** Perch terminals are created with `isTransient: true`, so VS Code does not revive them itself. That avoids duplicates and stale process environments. The extension keeps the IDs of sessions attached in this workspace in `workspaceState`. When the window opens again it reattaches those that are still running and detached (setting `perch.restoreTerminals`, default on). Sessions attached in another window are left there.

**Trust.** Opening, killing and listing sessions run only the bundled binary. In an untrusted workspace the extension still works, since none of its settings run workspace code. The binary path setting `perch.path`, for development, is machine-scoped and restricted in untrusted workspaces.

### API for other extensions

The extension's `exports` is `{ getApi(version: 1, callerId: string): PerchApiV1 | undefined }`, where `callerId` is the calling extension's ID. Asking for an unsupported version returns `undefined`, and callers fall back to their behavior without Perch. Raven and Sight find Perch with `vscode.extensions.getExtension('jbearak.perch')` and track installs and removals with `extensions.onDidChange`.

```ts
interface PerchApiV1 {
  readonly version: 1;
  /** False on Windows hosts or when the binary cannot run. */
  available(): Promise<boolean>;
  /** Starts a session in a new terminal and resolves when it is listening. */
  createSession(options: {
    name: string;
    kind: string;                      // 'r', 'stata', 'shell', ...
    command: string[];                 // {dir} and {id} expand
    cwd?: string;
    env?: Record<string, string>;      // {dir} and {id} expand
    annotations?: Record<string, string>;
    terminal?: { show?: boolean; location?: vscode.TerminalLocation; iconPath?: vscode.IconPath };
  }): Promise<{ session: SessionInfo; terminal: vscode.Terminal }>;
  listSessions(filter?: { owner?: string; kind?: string }): Promise<SessionInfo[]>;
  /** Opens (or focuses) a terminal attached to the session. */
  attach(id: string, options?: { show?: boolean }): Promise<vscode.Terminal>;
  /** The session a terminal is attached to, if it is a Perch terminal. */
  sessionForTerminal(terminal: vscode.Terminal): SessionInfo | undefined;
  send(id: string, text: string, options?: { paste?: boolean; submit?: boolean }): Promise<void>;
  kill(id: string, options?: { force?: boolean }): Promise<void>;
  annotate(id: string, set: Record<string, string>, unset?: string[]): Promise<void>;
  readonly onDidChangeSessions: vscode.Event<void>;
  /** Fires whenever Perch attaches a terminal to a session: on create, on open from the list, on restore. */
  readonly onDidAttach: vscode.Event<{ session: SessionInfo; terminal: vscode.Terminal }>;
}
```

Perch records `callerId` as the `owner` of sessions created through that API object. VS Code does not tell an extension who called its exports, so a caller can lie about its ID. That is acceptable: annotations are a convenience, not a security boundary, and every extension already runs with the user's full privileges.

## Integrations

These changes belong in Raven and Sight, not in Perch. They are listed so that Perch's API covers them.

**Sight (Stata).**
- Start Stata with `createSession({ kind: 'stata', command: [stataCli], ... })` when Perch is available, and keep the current terminal path otherwise.
- Replace `terminal.sendText('do "<file>"')` with `send(id, 'do "<file>"', { submit: true })`. Sends now work while the terminal is closed or attached in another window.
- On activation, adopt surviving Stata sessions from `listSessions({ owner: 'jbearak.sight' })` and from `onDidAttach`, so the "current Stata terminal" survives a window reopen.

**Raven (R).**
- Start the console with `command: [raven, 'console', '--ipc', '{dir}/ipc', ...]` and `annotations: { 'raven.console': 'ipc' }`. Raven keeps its own socket protocol with `queued`, `done` and `error`.
- The plot and data viewers read `RAVEN_SESSION_PORT` and `RAVEN_SESSION_TOKEN` from the environment at every request. A reopened window has a new server with a new port and token, but a running R keeps its old environment. Raven's bootstrap profile should read those two values from a file, `$PERCH_SESSION_DIR/raven-session.json`, when it exists, and fall back to the environment. On `onDidAttach` for a Raven session, the new extension host rewrites that file. No code is sent to R.
- httpgd's URL reaches Raven by a POST from R when the device starts. After a reopen Raven needs it again. Raven can store the URL as a Perch annotation when it first receives it, and read it back on attach.

## Platforms

| Host | Status |
|------|--------|
| Linux x64, arm64 (glibc or musl) | Supported. Static musl binary. |
| macOS x64, arm64 | Supported. macOS 11 or later. |
| Windows | Front end only, through Remote-SSH, WSL or Dev Containers to a supported host. |

## Security

- Directory and socket permissions as above, plus a peer uid check on every connection.
- No network listeners.
- `meta.json` holds the command line and working directory, readable only by the user.
- `send` and attach input go only to the session's PTY. The protocol has no operation that runs a command on the host.

## Known risks

From the brief, with where this design handles each:

| Risk | Handling |
|------|----------|
| Stale environment | Linked socket variables by default; `perch env` for the rest |
| Shell integration | Manual install works through the relay; replay strips stale marks; to be tested |
| Terminal queries while detached | Supervisor answers DSR and DA from its screen model |
| Cleanup | List with memory, kill with confirmation, status bar count, exited-session tombstones |
| HPC login nodes | Documented; `perch doctor` reports what it can detect |
| Reboots | Documented. Nothing survives. |
| Busy or idle | Time of last output and foreground process |
| Stata licenses | Documented: a detached Stata may hold a license seat |

New risks this design adds:

- **Two parsers on every byte of output.** vt100 and vte each parse everything the program writes. Both are fast compared with the cost of a terminal rendering it, but the plan must measure throughput on a large `cat`.
- **Restore fidelity.** vt100 does not know everything xterm.js knows. Programs using kitty keyboard or graphics protocols may not come back exactly. The emulator is behind an interface so it can be replaced.
- **Seam between history and screen.** History is replayed at the client's width, the screen is drawn from the model. Near the seam a line may appear twice or wrap differently. Accepted.

## Repository layout

```text
Cargo.toml                 workspace
crates/perch/              the binary (supervisor, client, CLI)
editors/vscode/            the extension
docs/                      design, plan, user docs
```

This mirrors Raven's layout. The license identifier is `GPL-3.0-only`, matching Raven's.

## Decisions on the brief's open questions

- **License identifier.** `GPL-3.0-only`, to match Raven, which declares `GPL-3.0` (the deprecated SPDX form of GPL-3.0-only).
- **Remote hosts the window is not connected to.** Not listed in v1, as the brief recommended. The extension runs on one host and sees that host.
- **Names and layout.** As above.
