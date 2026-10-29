# Manual verification checklist

These checks cannot be automated in Perch's CI or were not possible where Perch was built. Each one says what to do and what should happen. Status is "not run" until a maintainer runs it and records the date, the versions and the result.

| Check | Steps | Expected | Status |
|-------|-------|----------|--------|
| Remote-SSH survival | From VS Code, open a Remote-SSH window to a Linux host. Open a Persistent shell and an R session. Disconnect the network for longer than the reconnection grace period, or close the window. Reconnect. | Both sessions are listed and restore with their screens. | not run |
| VS Code server update | On a Remote-SSH host with live sessions, update VS Code so the server is replaced. Reconnect. | Sessions survive and the new extension attaches to them. | not run |
| Linked `VSCODE_IPC_HOOK_CLI` over Remote-SSH | In a Perch shell on a remote host, close the window, reopen it, reattach, run `code somefile`. | The file opens in the window now attached. | not run |
| Adoption after an extension host restart | With Perch terminals open, run "Developer: Restart Extension Host". | The same terminals stay and are adopted; no duplicates open. | not run |
| Shell integration after reattach | Install VS Code shell integration manually in zsh. In a Perch shell, run `sleep 30`, close the terminal, reopen the session during the sleep. | Command detection marks the command when it finishes. Record what happens either way. | not run |
| Restore in Terminal.app and iTerm | `perch attach --restore screen` into a session running `vim`, then quit `vim`. | Screen, cursor and the primary screen after `vim` are right; existing scrollback is kept. | not run |
| Registry installs | Install the published extension from the Marketplace and from Open VSX: locally on macOS and Linux, and from Windows and macOS through Remote-SSH to Linux. | The right platform build installs on the host that runs the workspace extension; the binary runs. | not run |
| Ad-hoc signature after registry download | On macOS, install the published extension from each registry and create a session. | The binary runs without a Gatekeeper prompt. | not run |
| darwin-x64 binary | On an Intel Mac, install the darwin-x64 VSIX and create, attach to and kill a session. | Works; CI only cross-compiles this target. | not run |
| Epilogue in other terminals | `perch attach` in Terminal.app, iTerm and tmux to a session running `vim` with mouse reporting on; detach with `Ctrl-\`. | The shell afterwards has no mouse reporting, no bracketed paste, a visible cursor and the main screen. | not run |
| Kitty keyboard stack after restore | In kitty or Ghostty, `perch attach --restore screen` to a session where a program pushed several kitty keyboard flags (including a zero entry), then pop them one by one. | Each pop leaves the same flags as without Perch. | not run |
| Stata | With Stata installed, run a long `do` file through Sight with Perch, close the window, reopen. | The session survives. Note whether the license seat stays held. | not run |
