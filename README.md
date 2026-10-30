# Perch

Persistent terminal sessions for VS Code and its open-source forks.

Perch keeps R, Stata and shell sessions running after you close the VS Code window or lose the SSH connection. When you come back, the terminal reopens with its screen and history. You can list, open and kill your sessions from any workspace on the host, or from a plain SSH login with the `perch` command.

- Works on macOS and Linux hosts, locally or through Remote-SSH, WSL and Dev Containers. On Windows, use it through a remote window to a macOS or Linux host.
- [Raven](https://github.com/jbearak/raven) (R) and [Sight](https://github.com/jbearak/sight) (Stata) can run their consoles in Perch sessions when Perch is installed. Perch also works on its own for shells and any other program.
- Perch never types into a session. Input comes only from the attached terminal, or from an explicit send by you or an extension.

## Install

- **From an extension registry:** install "Perch" (`jbearak.perch`) from the Visual Studio Marketplace or Open VSX, once it is published there. The registry picks the build for your host.
- **From a VSIX:** the repository's Release workflow builds one VSIX per platform (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `alpine-x64`, `alpine-arm64`) and a universal one, in its `vsix` artifact. Install the one for the host your workspace runs on, with "Extensions: Install from VSIX..." or `code --install-extension perch-<target>-<version>.vsix`. VS Code's command line does not check that a local VSIX matches the host, so pick the right file. The universal VSIX has no binary and is useful only as a front end on a host Perch does not run on, such as Windows.
- **The command line:** the extension bundles the `perch` binary and puts a copy on `PATH` in VS Code's terminals. For plain SSH logins, run "Perch: Show Command-Line Path" and add that directory to `PATH` in your shell startup file.

## Quick start

1. Open the terminal panel's profile menu (the arrow next to `+`) and pick **Persistent shell**, or run "Perch: New Persistent Shell".
2. Start something long, such as `R` or a build.
3. Close the terminal, or the window. The program keeps running.
4. Open the Perch view in the activity bar and click **Open** on the session. When you reopen the workspace, Perch reopens its sessions by itself.

From another terminal on the same host:

```sh
perch list
perch attach <name-or-id>    # Ctrl-\ detaches
```

## Documentation

- [Using Perch in VS Code](docs/usage.md): the Sessions view, terminals, settings, linked variables, what can end a session, troubleshooting and limits.
- [The `perch` command](docs/cli.md): every command, exit codes, the runtime directory and JSON output.
- [API for other extensions](docs/api.md): how Raven, Sight and other extensions run programs in Perch sessions.
- [Design](docs/design.md): how Perch works and why, compared with tmux, dtach, abduco, mosh, shpool and zmx.
- [Implementation plan](docs/plan.md) and [manual verification checklist](docs/verification.md).
- [Brief](docs/brief.md): the requirements Perch started from.

## Platforms

| Host | Binary | Tested |
|------|--------|--------|
| Linux x64, arm64 | Static musl binary; runs on old glibc distributions and on Alpine | Built and run in CI |
| macOS arm64 | macOS 11 or later | Built and run in CI |
| macOS x64 | macOS 11 or later | Built in CI by cross-compiling; not run |
| Windows | None | Front end only, through a remote window |

## How it works

Each session has its own supervisor process, which owns the program's pseudo-terminal. There is no central server: a crash affects one session, and supervisors started by different versions of Perch run side by side. The supervisor outlives the window and the login that started it, as long as the host lets user processes outlive their login (`perch doctor` checks).

A VS Code terminal shows a session by running `perch attach`, which passes bytes straight through, so VS Code's own scrollback, links and search work as usual. Alongside, the supervisor feeds the program's output to a screen model built on Ghostty's terminal library, `libghostty-vt`. When a terminal attaches, Perch redraws the screen, history and terminal modes from that model. While no terminal is attached, the model answers the cursor-position queries that programs such as R consoles send at a prompt.

Sessions live in `/tmp/perch-<uid>/`, one directory each, on the host where they run. A reboot ends them.

## License

Perch is licensed under the GNU General Public License, version 3 only (GPL-3.0-only). See [LICENSE](LICENSE).

The `perch` binary includes code from [Ghostty](https://github.com/ghostty-org/ghostty) and other projects under the MIT license. [NOTICE](NOTICE) lists them with their license texts.
