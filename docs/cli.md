# The `perch` command

`perch` starts sessions, lists them, attaches to them, sends input to them and ends them. The VS Code extension runs the same binary. This page covers every command.

- [Getting the binary](#getting-the-binary)
- [Naming a session](#naming-a-session)
- [Commands](#commands): [new](#perch-new), [attach](#perch-attach), [detach](#perch-detach), [list](#perch-list), [info](#perch-info), [send](#perch-send), [kill](#perch-kill), [dismiss](#perch-dismiss), [rename](#perch-rename), [annotate](#perch-annotate), [doctor](#perch-doctor), [version](#perch-version)
- [Exit codes](#exit-codes)
- [Runtime directory](#runtime-directory)
- [Environment variables](#environment-variables)
- [JSON output](#json-output)

## Getting the binary

The extension bundles `perch` and copies it to a stable place in VS Code's global storage, `<globalStorage>/bin/<os>-<arch>/perch`. New terminals in VS Code have that directory on `PATH` and `PERCH_BIN` set to the copy (setting `perch.addToPath`). To use `perch` over plain SSH, run "Perch: Show Command-Line Path" in VS Code and add the directory it shows to `PATH` in your shell startup file:

```sh
export PATH="$PATH:/home/me/.vscode-server/data/User/globalStorage/jbearak.perch/bin/linux-x64"
```

The path depends on the editor and the host. Use the one the command shows.

## Naming a session

Commands that take `<session>` accept, in this order:

1. the full 8-character ID, such as `k3x9q2ab`;
2. the exact name, such as `thesis`;
3. a prefix of the ID, such as `k3x`.

If a name or prefix matches more than one session, the command fails and lists the matching IDs.

## Commands

`perch help` (or `--help`, `-h`) prints a usage summary. A usage error prints the message and the summary and exits with 2.

### perch new

```text
perch new [options] -- <command> [args...]
```

Starts `<command>` in a new session. Without `-d` it then attaches the current terminal to the session, as `perch attach` does. The `--` is needed only when the command or its arguments start with `-`.

| Option | Effect |
|--------|--------|
| `-d`, `--detached` | Start the session without attaching. Prints the session ID once the program is running. |
| `--id ID` | Use this ID: 8 characters from `a-z` and `0-9`. By default Perch picks one. |
| `--name NAME` | The session's name. Default: the command's file name (`R` for `/usr/bin/R`). |
| `--kind KIND` | A free-form kind, such as `r`, `stata` or `shell`. Default: empty. |
| `--owner EXT` | The extension that owns the session, such as `jbearak.sight`. |
| `--annotate K=V` | An annotation. Repeat for more. |
| `--cwd DIR` | The program's working directory. A relative path is taken from the current directory. Default: the current directory. With `--cwd`, `PWD` is set to it. |
| `--env NAME=VALUE` | Set a variable in the program's environment. Repeat for more. |
| `--link-env NAME` | Link this variable (see [Linked variables](#linked-variables)). Repeat for more. Giving it replaces the default list. |
| `--no-link-env` | Link no variables. |
| `--scrollback LINES` | History lines the session keeps. Default 1000. Larger values are reduced to 10000. |
| `--label LABEL` | The client label this attach reports in `info`. Default: `tty:<terminal device>`, or `tty:none`. |
| `--detach-key KEY` | The detach key, or `none`. Default: `ctrl-\`. See [The detach key](#the-detach-key). |
| `--restore full\|screen` | How to restore the screen if this terminal re-attaches after another one took the session. Default: `screen`. |

The program gets the environment of `perch new`, then the `--env` values, then `PERCH_SESSION_ID` and `PERCH_SESSION_DIR`. In the command and in `--env` values, `{dir}` expands to the session directory and `{id}` to the session ID. `{{` and `}}` are literal braces. Any other brace is an error. The session directory exists, mode 0700, before the program starts.

The terminal size comes from the terminal `perch new` runs in, or 80 columns by 24 rows without one, limited to 1 to 500 in each direction.

Examples:

```sh
# Start R and attach to it.
perch new --name thesis --kind r -- R --no-save

# Start a build in the background and print its ID.
perch new -d --name build -- make -j8

# A program that listens on a socket in its session directory.
perch new -d --name console -- myprogram --socket '{dir}/ipc'
```

When `perch new` attaches, the program starts only after the attach completes, so its first output goes to your terminal. If the program cannot start, `perch new` prints why and exits with 127 (command not found) or 126 (any other reason).

### perch attach

```text
perch attach <session> [--label LABEL] [--if-detached] [--detach-key KEY|none]
                       [--restore full|screen] [--scrollback LINES]
```

Attaches the current terminal to a running session. Only one terminal is attached at a time. Attaching takes the session from any other terminal attached to it. That terminal prints `[perch] Opened in another window. Press Enter to take it back, or close this terminal.` and waits.

| Option | Effect |
|--------|--------|
| `--if-detached` | Attach only if no terminal is attached. Otherwise exit with 75 and leave the other terminal attached. |
| `--restore screen` | Redraw the screen without touching your terminal's scrollback. The default. |
| `--restore full` | Clear the terminal and its scrollback, then send the session's history and screen. Use it in a new, empty terminal. |
| `--scrollback LINES` | The most history rows a full restore sends. Default 1000, at most 10000. |
| `--label LABEL` | The client label `info` reports. Default: `tty:<terminal device>`. |
| `--detach-key KEY` | The detach key, or `none`. Default: `ctrl-\`. |

You cannot attach a session from inside itself. `perch attach` refuses when `PERCH_SESSION_ID` names the session and the process really runs in it.

When the attach ends, `perch attach` resets the terminal (it leaves the alternate screen, turns off mouse reporting and bracketed paste, and shows the cursor), then prints one of:

- `[perch] Detached.` and exits with 0;
- `[perch] The program ended: exit code 0.` and exits with the program's status;
- `[perch] Lost the connection to the session.` and exits with 1.

If the program was killed by a signal, the exit status is 128 plus the signal's number.

#### The detach key

The detach key is `Ctrl-\` by default, as in dtach and abduco. In a normal terminal `Ctrl-\` sends SIGQUIT, which would kill R, so catching it also removes that hazard. `--detach-key` takes `ctrl-<letter>` (also written `c-x` or `^X`), `ctrl-\`, `ctrl-]`, `ctrl-^`, `ctrl-_`, `ctrl-@`, `ctrl-[` or `none`. With `none` every key goes to the program. Perch recognizes the key in its plain form and in the forms terminals send with the kitty keyboard protocol or modifyOtherKeys.

```sh
perch attach thesis
perch attach thesis --detach-key ctrl-]
perch attach k3x9q2ab --if-detached || echo "attached elsewhere"
```

### perch detach

```text
perch detach [<session>]
```

Detaches whatever terminal is attached to the session. Without `<session>` it detaches the session it runs in, so typing `perch detach` at a shell prompt inside a session detaches you. It fails if no terminal is attached.

### perch list

```text
perch list [--json] [--all]
```

`ls` is an alias. Lists running sessions. `--all` (or `-a`) adds sessions that have ended: exited and lost ones. Ended sessions older than 7 days are removed when `list` runs.

```text
$ perch list --all
ID        NAME   KIND   STATE                 AGE  CWD
gfqv2wty  sh            exited (exit code 3)  1m   ~/project
t1qhkity  build  shell  running               5m   ~/project
```

States:

| State | Meaning |
|-------|---------|
| `starting` | The session exists; its program has not started yet. |
| `running` | The program is running. |
| `unresponsive` | The session's supervisor holds its lock but did not answer within 500 ms. |
| `exited` | The program ended while no terminal was attached. Its last screen is kept. |
| `lost` | The supervisor ended without recording an exit, for example because it was killed. |

A program that ends while a terminal is attached leaves nothing behind: the terminal saw it end.

`--json` prints the fields described in [JSON output](#json-output).

### perch info

```text
perch info <session> [--json]
```

Shows one session, including ended ones.

```text
$ perch info build
id:          t1qhkity
name:        build
kind:        shell
state:       running
pid:         48211
cwd:         /home/me/project
dir:         /tmp/perch-1000/t1qhkity
command:     sh -c make -j8
created:     2026-10-30T12:14:53Z
host:        login1
size:        80x24
last output: 2026-10-30T12:19:02Z
memory:      about 3.2 MB
foreground:  make (48230)
model:       ok
```

`memory` is the summed resident memory of the program and every process in its terminal session. Shared pages are counted once per process, so it is an estimate. `cwd` is the program's current directory if Perch can read it, else the last directory the program reported with OSC 7, else the directory it started in. `model` is the state of the screen model Perch keeps for restores: `ok`, `partial` or `failed` (see [Limits](usage.md#limits)).

### perch send

```text
perch send <session> [--text TEXT | --stdin] [--paste] [--submit]
```

Queues input for the session's program, as if typed. It works whether or not a terminal is attached. Your keystrokes and sends share one queue, so a send is never mixed with typed input.

| Option | Effect |
|--------|--------|
| `--text TEXT` | The text to send. |
| `--stdin` | Send everything read from standard input. |
| `--paste` | Wrap the text in bracketed-paste markers, but only if the program has turned bracketed paste on. |
| `--submit` | Press Enter after the text: append a carriage return. With no text, send only Enter. |

The queue holds 1 MiB. Perch sends exactly the bytes you give it; it adds nothing and runs nothing on its own.

| Exit | Meaning |
|------|---------|
| 0 | Queued. The bytes will be written in order unless the session ends first. |
| 3 | The input queue is full. Nothing was sent. |
| 4 | The connection dropped before the session answered. The input may or may not have been sent. Do not send it again blindly. |
| 1 | Nothing was sent: no such session, it has ended, the input is over 1 MiB, or the session could not be reached. |

```sh
perch send stata --text 'do "analysis.do"' --submit
printf 'summary(fit)\n' | perch send thesis --stdin
```

### perch kill

```text
perch kill <session> [--force] [--pid]
```

Ends the program and every process in its terminal session, background and stopped jobs included. Perch sends SIGHUP and SIGCONT, as closing a terminal window does, then SIGKILL to anything left after 5 seconds. `--force` sends SIGKILL at once. `perch kill` returns once the session has ended, and leaves no entry behind.

Processes that left the terminal session with their own `setsid` (daemons, `nohup setsid ...`) are not killed.

`--pid` is for a session whose supervisor does not answer. It signals the program's terminal session and then the supervisor directly, using the process IDs recorded in the session's `meta.json`. Before signaling a process it checks that the process belongs to you and has the start time that was recorded, so a reused process ID is left alone.

Killing a session that has already ended fails; use `perch dismiss`.

### perch dismiss

```text
perch dismiss <session>
```

Removes an ended (`exited` or `lost`) session and its last output. It refuses a running session.

### perch rename

```text
perch rename <session> <name>
```

Changes a running session's name. The name must not be empty.

### perch annotate

```text
perch annotate <session> K=V... [--unset K]...
```

Sets and removes annotations on a running session: free-form strings that integrations use to mark their sessions. Annotations can be read by anyone who can list your sessions and by every VS Code extension, so do not put secrets in them.

```sh
perch annotate thesis project=chapter3 --unset draft
```

### perch doctor

```text
perch doctor [--json]
```

Checks whether sessions will work and survive on this host. Each line is a status (`ok`, `info`, `warn` or `fail`), a check name and a message.

| Check | What it checks |
|-------|----------------|
| `runtime-dir` | The runtime directory exists or can be created, is a real directory owned by you, mode 0700. |
| `runtime-dir-location` | Shown only when the runtime directory is inside `$XDG_RUNTIME_DIR`, which logind removes when your last login ends. |
| `socket-path` | Socket paths fit the system's limit (103 bytes on macOS, 107 on Linux). |
| `flock` | `flock` on a directory there really locks. It may not on NFS. |
| `cgroup` (Linux) | Where processes started from here run: a login session's scope, your service manager, or a systemd service whose stop would end every session started from it. |
| `logind` (Linux) | Whether logind's `KillUserProcesses` applies to you, and whether linger is on. |
| `container` (Linux) | Shown when running in a container: sessions end when it stops. |
| `host` (macOS) | macOS has no logind. Sessions outlive the login that started them, but not a reboot. |

After the checks it says when sessions are not expected to survive logout, or when it could not tell. `perch doctor` exits with 1 if any check failed, else 0.

```text
$ perch doctor
ok    runtime-dir: /tmp/perch-1000: a directory of yours, mode 0700
ok    socket-path: socket paths are 29 bytes, within the limit of 107
ok    flock: flock on a directory in the runtime directory works
info  cgroup: /user.slice/user-1000.slice/session-12.scope: a login session's scope, which logind stops at logout if KillUserProcesses applies
ok    logind: KillUserProcesses=no does not apply to me (loginctl)
```

### perch version

```text
perch version [--json]
```

Prints the version, the Ghostty commit Perch was built with, and the result of a self-test of its terminal library. `perch --version` and `perch -V` do the same. It exits with 1 if the self-test fails.

```text
$ perch version
perch 0.1.0 (Ghostty 521cf2c, selftest ok)
```

## Exit codes

| Code | Commands | Meaning |
|------|----------|---------|
| 0 | all | Success. For `attach` and `new`: detached, or the program exited with 0. |
| 1 | all | An error, printed as `perch: <message>`. For `attach` and `new`: the connection to the session was lost. For `doctor`: a check failed. |
| 2 | all | A usage error. |
| 3 | `send` | The input queue is full; nothing was sent. |
| 4 | `send` | The connection dropped; the input may not have been sent. |
| 75 | `attach --if-detached` | Another terminal is attached; nothing changed. |
| 126, 127 | `new` | The program could not be run (126) or was not found (127). |
| other | `attach`, `new` | The program's own exit status, or 128 plus the number of the signal that killed it. |

A program can exit with 3, 4 or 75 itself. The extension checks `perch info` after an exit with 75 to tell the two apart.

## Runtime directory

Sessions live in `/tmp/perch-<uid>/`, or in `$PERCH_RUNTIME_DIR` if it is set (a relative path is taken from the current directory). Each session has its own directory there, named by its ID:

```text
/tmp/perch-1000/               mode 0700
  k3x9q2ab/                    the session directory, {dir}; mode 0700
    sock                       the supervisor's socket
    meta.json                  the session's metadata
    supervisor.log             the supervisor's log, at most 256 KiB
    env/                       links for linked variables
    tail.txt                   the last screen and history, written when the program exits while detached
```

Every `perch` process refuses a runtime directory that is a symbolic link, is not owned by you, or does not have mode 0700, and tells you to set `PERCH_RUNTIME_DIR`. It creates the directory if it is missing.

The directory is in `/tmp` because it must be on the local machine (a shared home directory over NFS would show one node's sessions on another), the socket paths must be short, and `$XDG_RUNTIME_DIR` is removed at logout. A reboot clears it. Each supervisor touches its files every hour, so cleaners such as `systemd-tmpfiles` that remove old files in `/tmp` leave live sessions alone.

Other programs can keep their own files in a session directory; Raven keeps its console socket there. If the directory is removed while the session runs, the supervisor recreates it within a minute and the session's `degraded` list gains `directory-recreated`. Files other programs kept there are gone.

## Environment variables

| Variable | Set by | Meaning |
|----------|--------|---------|
| `PERCH_RUNTIME_DIR` | you | Where sessions live. Default `/tmp/perch-<uid>`. Every `perch` process, the extension included, must see the same value to see the same sessions. |
| `PERCH_SESSION_ID` | Perch, inside a session | The session's ID. `perch detach` uses it. |
| `PERCH_SESSION_DIR` | Perch, inside a session | The session directory, the same path as `{dir}`. |
| `PERCH_BIN` | the extension, in VS Code terminals | The path of the command-line copy of `perch`. |

`PERCH_SESSION_ID` is trusted only when the process really runs in that session. A process that left with its own `setsid` keeps the variable, but `perch detach` and `perch attach` ignore it there.

### Linked variables

Some variables hold the path of a socket that belongs to whoever is connected: `SSH_AUTH_SOCK` for the SSH agent, `VSCODE_IPC_HOOK_CLI` for the `code` command in a remote VS Code window. In a session that outlives the connection, those paths go stale.

For each linked variable that is set when the session starts, the program sees `$PERCH_SESSION_DIR/env/<NAME>` instead of the real path. That is a symbolic link, and each attach points it at the attaching terminal's own value. Programs inside the session reach the agent or window of whoever is attached now. If the attaching terminal does not have the variable, the link points at a path that does not exist, so programs fail as they would with a stale value. Detaching leaves the links alone. A variable that is not set when the session starts stays unset.

`perch new` links `SSH_AUTH_SOCK` by default. `--link-env NAME` replaces the list, and `--no-link-env` links nothing:

```sh
perch new --link-env SSH_AUTH_SOCK --link-env VSCODE_IPC_HOOK_CLI -- zsh
```

## JSON output

Every `--json` output is one JSON value. Top-level documents carry `"schema": 1`. Fields may be added in later versions, and readers should ignore fields they do not know.

### list --json and info --json

`perch list --json` prints `{"schema": 1, "sessions": [...]}`. `perch info --json` prints one session object, without a `schema` field. A session object has:

| Field | Type | Meaning |
|-------|------|---------|
| `format` | number | The `meta.json` format, 1. |
| `id` | string | The session ID. |
| `name` | string | The name. |
| `kind` | string | The kind; may be empty. |
| `owner` | string | The owning extension. Absent if none. |
| `annotations` | object | Annotations, string to string. |
| `command` | string array | The command, placeholders expanded. |
| `cwd` | string | The working directory: live for a running session that answered, else the starting one. |
| `dir` | string | The session directory. |
| `host` | string | The host name. |
| `createdAt` | string | When it started, RFC 3339 in UTC. |
| `endedAt` | string | When it ended, for `exited` and `lost`. |
| `state` | string | `starting`, `running`, `unresponsive`, `exited` or `lost`. |
| `exit` | object or null | How the program ended: `code`, or `signal` (such as `SIGKILL`) and `signalNumber`, or `error` if it never started. |
| `attached` | boolean | Whether a terminal is attached. |
| `clientLabel` | string or null | The attached terminal's label. |
| `clientPid` | number or null | The attached client's process ID, if known. |
| `pid` | number or null | The program's process ID. |
| `pidStart` | string or null | The program's start time, as Perch records it. |
| `supervisorPid` | number | The supervisor's process ID. |
| `supervisorStart` | string | The supervisor's start time. |
| `perchVersion` | string | The version of the `perch` that started the session. |
| `protocol` | object | `min` and `max` protocol versions the supervisor speaks. |
| `scrollback` | number | History lines the session keeps. |
| `linkedEnv` | string array | The names of linked variables that were set at start. |
| `degraded` | string array | Problems the session has had, such as `directory-recreated`. |
| `rows`, `cols` | number | The terminal size. |
| `lastOutputAt` | string or null | When the program last wrote output. |
| `lastInputAt` | string or null | When the program last received input. |
| `memoryBytes` | number | Resident memory of the program's terminal session, an estimate. |
| `foreground` | object | The foreground process group: `pid` and `name`. |
| `model` | string | The screen model: `ok`, `partial` or `failed`. |

`rows` through `model`, and `clientLabel` and `clientPid`, come from the supervisor and are present only for a running session that answered. `attached` is always present.

### doctor --json

| Field | Type | Meaning |
|-------|------|---------|
| `schema` | number | 1. |
| `runtimeDir` | string | The runtime directory checked. |
| `ok` | boolean | False if any check failed. |
| `survivesLogout` | boolean or null | Whether sessions started here are expected to outlive logout; null if Perch could not tell. |
| `checks` | array | Each check: `name`, `status` (`ok`, `info`, `warn`, `fail`) and `message`. The `cgroup` check adds `cgroup`; the `logind` check adds `killUserProcesses`, `killOnlyUsers`, `killExcludeUsers` and `linger`. |

### version --json

`version` (string), `ghostty_commit` (string) and `selftest`, an object with `ok` and the self-test's details.
