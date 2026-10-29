# Perch brief

This file records the requirements and context from the conversation that started the project. It is the input to the design, not the design.

## Problem

Raven (R) and Sight (Stata) run their sessions in a VS Code terminal. Closing the window, or losing a remote connection, kills the session and the user's in-memory work. The workaround today is to start R or Stata inside tmux and send code to that terminal. Raven's `docs/r-console.md` documents this workaround.

## Goal

A separate companion extension plus a host binary that keeps sessions alive outside the editor. The same machinery also provides persistent shell sessions.

## Requirements

- **Separate extension.** Raven and Sight use Perch only when it is installed, so their behavior does not change without it. Detection goes through `vscode.extensions.getExtension(...)` and `extensions.onDidChange`. The extension exposes a versioned API through its `exports`.
- **Session list.** Show every session the user opened and has not closed, on the host the window is connected to. The list updates when a window reconnects.
- **Kill.** Terminate a session from the list, with a confirmation that shows name, kind, working directory, age and memory.
- **Open from anywhere.** Attach to a session from any window without switching workspaces.
- **Persistent shells.** A "Persistent shell" terminal profile built on the same host.
- **Platforms.** The host binary runs on macOS and Linux. Windows needs only the editor front end, which reaches a Linux or macOS host over remote development.
- **No code injection.** Perch never writes code into a session on its own. It sends only what the user or an integrating extension explicitly asks it to send. It does not run `set more off`, load packages, or answer prompts such as `--more--`.

## Starting design ideas

These came out of discussion and are for the design to confirm or reject.

- One supervisor process per session, with no central server. A session records itself in a state directory under `~/.local/state/perch/` as a metadata file plus a Unix socket. `list` scans the directory and drops stale entries.
- The supervisor owns a PTY, started with `setsid`, runs any command inside it, and keeps a ring buffer of recent output.
- Subcommands along the lines of `new -- <command>`, `attach`, `list --json`, `kill` and `send`.
- `send` writes text to the PTY master through the supervisor socket, so editors can send code while no terminal is attached.
- Sockets are mode 0600 inside a 0700 directory. Keep socket paths short, because the Unix socket path limit is about 104 bytes on macOS.
- One attached client at a time in v1. A new attach takes over.
- A protocol version in the first message, because supervisors outlive extension updates and an update deletes the old extension directory with its binary.
- Metadata can carry named endpoints. A `{ipc}` placeholder in a launch command lets the host fill in a socket path, so Raven can keep its own console socket.
- Ring-buffer replay first, with a SIGWINCH repaint. Later, track real screen state with libghostty-vt, which matters most for full-screen programs like `vim` and `less`. libghostty is pre-1.0, so pin a commit.
- Ship the binary as platform-specific VSIX targets for macOS and Linux on x64 and arm64, and publish to Open VSX. Check that Open VSX handles target platforms before committing.

## Known risks

- **Stale environment.** After a reconnect, variables such as `SSH_AUTH_SOCK` and `VSCODE_IPC_HOOK_CLI` in a long-lived shell point at dead sockets. A command that writes a script to source, in the spirit of tmux's `update-environment`, is one option. The user sources it, and Perch never injects it.
- **Shell integration.** VS Code injects shell integration only into shells it launches itself. Manual installation may work through the relay. This is untested.
- **Terminal queries.** A line editor may ask the terminal for the cursor position when it draws a prompt. With no client attached, nothing answers. Test whether prompts stall while detached.
- **Cleanup.** Detached R sessions can hold gigabytes. Provide list and kill, and surface sessions in the UI.
- **HPC login nodes.** Some kill or limit long-lived processes. Perch cannot fix that.
- **Reboots.** Nothing survives a reboot or a killed supervisor.
- **Busy or idle status.** Raven's console socket reports `done` and `error`. A plain Stata PTY has no equivalent, so show the time of last output instead.
- **Stata licenses.** A detached Stata process may hold a license seat, depending on the license type.

## Integration context

- **Raven.** `raven console` is a Rust program that embeds R in its own process, with a reedline editor on top, and runs in a VS Code terminal. Code from the editor goes to a Unix socket, not through the terminal. Look at `crates/raven/src/console/ipc.rs`, `crates/raven/src/console/mod.rs`, `editors/vscode/src/send-to-r/r-terminal-manager.ts` and `docs/r-console.md` on `main` in `jbearak/raven`. The plot viewer and data viewer rely on per-terminal wiring, such as the plot FIFO and terminal environment variables. A reopened window has a new extension host, so those pieces need to reconnect to a surviving session. That wiring has not been read closely yet.
- **Sight.** Sight opens a terminal running the Stata CLI and types into it. It has no socket. Perch's `send` is what replaces that typing.

## Constraints

- Any code studied or reused must be under a license compatible with GPL-3. MIT, BSD, ISC and Apache-2.0 qualify. GPL-2-only and source-available licenses such as Elastic do not. The original request said "GPT-3 compatible", read here as GPL-3. Confirm with the author.
- Do not study Positron. It is source-available but proprietary.
- Perch's own license is undecided. Ask the author before adding a LICENSE file.
- The extension must work on stock VSCodium and Open VSX as well as on Rhodium, a planned VS Code OSS fork.

## Open questions

- Perch's license.
- Whether to list sessions on remote hosts a window is not connected to. The current recommendation is no, for v1.
- Names and layout of the repo, for example a Rust crate for the host and a TypeScript extension.
