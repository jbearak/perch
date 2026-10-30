# Using Perch in VS Code

Perch keeps terminal sessions running after the VS Code window closes or the SSH connection drops. This page covers the extension. For the `perch` command, see [cli.md](cli.md).

## Where it runs

Perch runs on the host where your workspace is: the remote host in a Remote-SSH, WSL or Dev Containers window, or your own machine in a local window. The host must run macOS or Linux. In a local window on Windows, the Perch view says that Perch needs a macOS or Linux host, and nothing else happens.

## The Sessions view

The Perch icon in the activity bar opens the Sessions view. Its title names the host, such as "Sessions on login1". On a cluster behind a load-balanced name, that tells you why sessions from another login node are missing: sessions belong to the node they run on.

The view lists every session on the host for your user, from any workspace and including sessions started with the `perch` command. Running sessions come first, newest first, then ended ones. Each item shows the name and a line such as:

```text
R · ~/thesis · 3h · about 4.1 GB · detached
```

That is the kind, the working directory, the age, the memory of the program and everything it started, and the state: `attached here`, `attached` (in another window or terminal), `detached`, `exited, exit code 0`, `lost`, `starting` or `unresponsive`. A session with a problem shows `warning: directory-recreated` and a warning icon.

Actions on a running session:

- **Open** (terminal icon): opens a terminal attached to the session. If the session is attached in another window, Perch asks "R: thesis is open in another window. Move it here?" first. The other window's terminal stays open and says the session moved.
- **Kill** (trash icon): asks for confirmation, showing the kind, directory, age and memory, then ends the program and everything it started in its terminal.

Actions on an ended session (`exited` or `lost`):

- **Show Last Output**: opens the session's last screen and history as text, with how it ended. A lost session has no saved output; the document points to its `supervisor.log`.
- **Dismiss**: removes the entry.

Ended sessions are removed after 7 days anyway.

The list refreshes when the window gains focus, when the view becomes visible, after Perch's own actions, every 5 seconds while the view is visible and every 30 seconds while it is hidden. The refresh button refreshes it at once.

The status bar shows the number of detached sessions, when there are any. Clicking it lists them; pick one to open it, or use the buttons to kill it or show it in the view.

## Commands

| Command | What it does |
|---------|--------------|
| Perch: New Persistent Shell | Starts a shell in a new session and opens its terminal. |
| Perch: Open | Opens a running session, chosen from a list. |
| Perch: Show Detached Sessions | The same list as the status bar item. |
| Perch: Refresh | Refreshes the Sessions view. |
| Perch: Show Command-Line Path | Shows where the command-line copy of `perch` is, with a button to copy the path. |

## Persistent shells

"Persistent shell" is a terminal profile. Pick it from the terminal panel's profile menu (the arrow next to `+`), or run "Perch: New Persistent Shell". The shell starts in the workspace's first folder. It is the program in `perch.shell`, or `$SHELL` if that is empty, with `-l` on macOS to match VS Code's default there. Perch does not use your default VS Code terminal profile.

Persistent shells and the terminals Perch opens are ordinary VS Code terminals running `perch new` or `perch attach`. Scrollback, selection, links and search work as in any terminal.

VS Code's shell integration (command decorations, command navigation) is not injected into a Perch terminal, because VS Code injects it only into shells it starts itself. You can install it by hand in your shell's startup file, as VS Code's documentation describes under "Manual installation".

## Closing, reopening and restoring

**Closing a Perch terminal detaches it.** The program keeps running. The first time, Perch says so, with buttons to kill the session or not show the message again. Reopen the session from the Sessions view.

**When the window opens again,** Perch reopens the terminals of the sessions that were attached in this workspace and are now detached, with their screen and history. This happens after a window reload, after reopening the folder, and after a dropped Remote-SSH connection. It does not reopen:

- sessions whose terminal you closed yourself;
- sessions attached in a window of another workspace (Perch never takes a session from a live terminal when restoring);
- anything in a window with no folder or workspace open.

Restores are retried for 60 seconds after the window opens, which covers a window whose previous instance is still letting go of its sessions.

The setting `perch.restoreTerminals` turns restoring off.

**Opening a session in a second window** moves it there, after you confirm. Only one terminal is attached to a session at a time.

## Surviving a closed window or a dropped connection

A session's program runs under its own small supervisor process, outside VS Code. Closing the window, losing the SSH connection, the Remote-SSH reconnection grace period running out, and a restart or update of the VS Code server all leave it running. So does an update of the Perch extension: the running supervisor keeps the binary it started with, even after the update deletes it.

### What can still end a session

- **A reboot** of the host.
- **Killing the supervisor**, or the out-of-memory killer ending the program.
- **logind with `KillUserProcesses=yes`.** On Linux, when your login ends, logind can stop every process it started. Most distributions set `no`; some clusters set `yes`.
- **A systemd service stopping.** Sessions started from inside a service (a `code tunnel` user service, for example) end when that service stops or restarts.
- **Your user's service manager stopping** when your last login ends, if sessions were started from it and linger is off.
- **A container stopping**, such as a Dev Container that stops when its window closes.
- **Cluster reapers** that kill long-running or large processes on login nodes.
- **Removing the runtime directory.** A cleaner such as `systemd-tmpfiles` leaves live sessions alone, because each supervisor touches its files every hour.

When Perch starts on a host it runs `perch doctor`. If sessions there are not expected to survive logout, or a check fails, it shows a warning once per host. Run `perch doctor` in a terminal to see the details; [cli.md](cli.md#perch-doctor) explains each check. Perch cannot change host policy. On Linux, `loginctl enable-linger` keeps your service manager running after logout where your site allows it.

Stata note: a detached Stata session may keep holding a license seat while it runs.

## Plain SSH and other terminals

A session started in VS Code can be reached from any terminal on the same host, for example over SSH from another computer:

```sh
ssh login1
perch list
perch attach thesis
```

`perch attach` takes the session from any VS Code window it is attached in; that terminal says the session moved and waits for Enter to take it back. Press `Ctrl-\` to detach and leave the program running. `perch detach`, typed at a prompt inside the session, does the same.

For `perch` to be on `PATH` in plain SSH logins, run "Perch: Show Command-Line Path" and add the directory it shows to `PATH` in your shell startup file. Inside VS Code terminals it is already on `PATH`.

By default `perch attach` redraws the screen but keeps your terminal's scrollback. `perch attach --restore full` also sends the session's history, after clearing the terminal. See [cli.md](cli.md) for every command.

## Settings

| Setting | Default | Effect |
|---------|---------|--------|
| `perch.restoreTerminals` | `true` | Reopen this workspace's detached Perch sessions when the window opens. |
| `perch.shell` | `""` | The program a persistent shell runs: a path, or an array of a path and its arguments, such as `["/bin/zsh", "-l"]`. Empty means `$SHELL` (or `/bin/sh` if that is unset), with `-l` on macOS. |
| `perch.linkedEnvironment` | `["SSH_AUTH_SOCK"]` | Environment variables that hold socket paths and are re-pointed at each attach. See [Linked variables](#linked-variables). An empty list links nothing. Applies to new sessions. |
| `perch.addToPath` | `true` | Append the directory of Perch's command-line copy to `PATH`, and set `PERCH_BIN`, in every new terminal in the editor, not only Perch's. A `perch` already on your `PATH` still comes first. |
| `perch.path` | `""` | A `perch` binary to run instead of the bundled one. A leading `~/` means your home directory. Empty uses the bundled binary. |

`perch.shell`, `perch.linkedEnvironment` and `perch.path` are machine settings: set them in the remote host's settings for a remote window.

The size of a new session's history comes from `terminal.integrated.scrollback` (VS Code's default is 1,000 lines), at most 10,000.

## Linked variables

A session can outlive the connection it started in. Variables that point at that connection's sockets then go stale: `SSH_AUTH_SOCK` points at an SSH agent that is gone, and in a remote window `VSCODE_IPC_HOOK_CLI` points at the old window, so `code file.txt` fails.

For each variable in `perch.linkedEnvironment` that is set when the session starts, the program sees a path inside the session directory instead of the real one. That path is a link, and each attach points it at the attaching terminal's own value. So `ssh` and `git` inside a session use the agent of whoever is attached now, after a reconnect too. If the attaching terminal does not have the variable, programs fail as they would with a stale value. A variable that is not set when the session starts stays unset.

- `SSH_AUTH_SOCK` is linked by default. It has been checked with SSH agent forwarding.
- `VSCODE_IPC_HOOK_CLI` is not linked by default. Add it to `perch.linkedEnvironment` so that `code` inside a session opens files in the window now attached. It is off until it has been checked over Remote-SSH, which has not been done yet. VS Code sets it only in remote windows.

Other variables, such as `DISPLAY`, are not updated.

## Troubleshooting

**The view says "Perch needs a macOS or Linux host".** You are in a local window on Windows. Open a Remote-SSH, WSL or Dev Containers window to a macOS or Linux host.

**My sessions are missing.** Sessions belong to one host and one user. Check the host name in the view's title: a cluster's login alias may have put you on another node. Sessions in a non-default `PERCH_RUNTIME_DIR` are listed only by a `perch` that sees the same value; the extension uses the value in its own environment.

**A session is `lost`.** Its supervisor ended before the program did, for example because something sent the supervisor `kill -9`. Show Last Output names its `supervisor.log`, which may say why.

**A session is `unresponsive`.** Its supervisor holds the session's lock but does not answer. Wait and refresh. If it stays that way, `perch kill --pid <id>` signals its processes directly.

**Sessions end when I log out.** Run `perch doctor`. A `warn` line names the cause, usually logind's `KillUserProcesses` or sessions started from a service.

**`perch` refuses the runtime directory.** `/tmp/perch-<uid>` must be a directory you own with mode 0700. If another user created it, set `PERCH_RUNTIME_DIR` to a directory of your own, on a local disk, in your shell startup file, so VS Code and your terminals all see it.

**The screen did not come back.** The terminal shows `[perch] The screen could not be restored. Asking the program to redraw.` See [Limits](#limits). In most shells `Ctrl-L` redraws the screen.

**The Perch output channel** (View: Output, then "Perch") logs what the extension did: binaries, restores, `perch doctor` results and errors.

## Limits

**Restore fidelity.** Perch keeps a model of each session's screen and history, using Ghostty's terminal library, and redraws from it when a terminal attaches. These do not come back exactly:

- Images (sixel, kitty graphics), double-width and double-height lines, and other state the model does not write out.
- Settings the program saved with `CSI ? s` to restore later.
- The alternate screen when you attach while the program is on the main screen. A program that switches back to the alternate screen with mode 47 then finds it empty in your terminal, where it would have found its old contents.
- Some sequences that Ghostty and VS Code's terminal interpret differently, such as invalid UTF-8.
- History is limited by the session's scrollback, the attaching terminal's scrollback, and 4 MiB. A line that runs from history onto the screen is split where they meet, and a later resize does not rejoin it.
- After a terminal stalls (stops reading for 5 seconds), Perch redraws the screen when it reads again, but lines that scrolled past meanwhile are missing from its scrollback.

**When the model fails.** If the screen model crashes, falls far behind or uses too much memory, the program keeps running and Perch starts a new model, at most 3 times an hour. A new model has not seen earlier output, so it is `partial` (`perch info` shows it): attaching then prints a notice and asks the program to redraw by resizing the terminal. A partial model becomes complete only when the program resets the terminal.

**Programs waiting for an answer while detached.** With no terminal attached, Perch answers the cursor-position and device-attribute queries that programs such as Raven's console and fish send at their prompt. It does not answer color or feature queries; a program that waits for one waits for its own timeout. After a model restart Perch cannot answer cursor-position queries until the model is complete, so a detached Raven console then waits about 2 seconds at each prompt.

**History and memory.** A session keeps at most 10,000 lines of history. The model holds about 19 MB for 10,000 lines at 120 columns, and about 62 MB at 500 columns.

**One terminal at a time.** Opening a session elsewhere moves it.

**Programs that detach themselves.** Processes that leave the session's terminal with their own `setsid` are not killed with it.
