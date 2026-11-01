# The Perch API for other extensions

Other VS Code extensions can run their programs in Perch sessions, so that a REPL, a server or a long job they start survives a closed window or a dropped connection. This page describes version 1 of the API.

Perch never types into a session on its own. Input reaches a session only from its attached terminal, or from `send` when an extension or user asks for it.

## Finding Perch

Perch's extension ID is `jbearak.perch`. It is a workspace extension: in a remote window it runs in the remote extension host, next to the sessions. VS Code does not pass an extension's exports between extension hosts, so an extension that uses the API must run in the same host as Perch. In a remote window that means it must run as a workspace extension too; a UI extension running locally cannot use it.

Perch activates on startup, before or after your extension, and it may be installed or removed while your extension runs. Events fired before you subscribe are lost. So subscribe to the events first, then take the snapshots with `terminals()` and `listSessions()`. An attach or end can arrive while the listing is in flight; the event is newer, so skip listing entries for sessions an event changed meanwhile. Look Perch up again whenever `vscode.extensions.onDidChange` fires, and discard the results of a lookup that a later one has replaced. `connectPerch` below does all of this. It builds the new state aside and swaps it in only when the lookup succeeds, so your sessions do not vanish while it runs. It never rejects, so `void connectPerch()` leaves no unhandled rejection.

```ts
import * as vscode from 'vscode';
import type { PerchApiV1, SessionInfo } from './perch-api'; // a copy of the types below

const callerId = 'publisher.my-extension'; // your own extension ID

async function getPerch(): Promise<PerchApiV1 | undefined> {
  const ext = vscode.extensions.getExtension<{ getApi(version: number, callerId: string): unknown }>('jbearak.perch');
  if (ext === undefined) return undefined;
  const exports = ext.isActive ? ext.exports : await ext.activate();
  const api = exports?.getApi(1, callerId) as PerchApiV1 | undefined;
  if (api === undefined || !(await api.available())) return undefined;
  return api;
}

// What you know about your sessions. Each lookup builds a new one and swaps it in.
interface Tracked {
  sessions: Map<string, SessionInfo>;
  terminals: Map<string, vscode.Terminal>;
}
let tracked: Tracked = { sessions: new Map(), terminals: new Map() };
let perch: PerchApiV1 | undefined;
let perchEvents: vscode.Disposable | undefined;
let generation = 0; // which connectPerch() call is current

function seen(t: Tracked, session: SessionInfo, terminal?: vscode.Terminal): void {
  if (session.state === 'exited' || session.state === 'lost') return ended(t, session);
  t.sessions.set(session.id, session);
  if (terminal !== undefined) t.terminals.set(session.id, terminal);
}

function ended(t: Tracked, session: SessionInfo): void {
  t.sessions.delete(session.id);
  t.terminals.delete(session.id);
}

async function connectPerch(): Promise<void> {
  const current = ++generation;
  let events: vscode.Disposable | undefined;
  try {
    const api = await getPerch();
    if (current !== generation) return; // a later call, or disposal, took over
    const next: Tracked = { sessions: new Map(), terminals: new Map() };
    if (api !== undefined) {
      // 1. Subscribe first. While the listing runs, note what events change.
      let changedByEvents: Set<string> | undefined = new Set();
      events = vscode.Disposable.from(
        api.onDidAttach(({ session, terminal }) => {
          changedByEvents?.add(session.id);
          if (session.owner === callerId) seen(next, session, terminal);
        }),
        api.onDidEndSession((session) => {
          changedByEvents?.add(session.id);
          ended(next, session);
        }),
      );
      // 2. Then take the snapshots.
      for (const { session, terminal } of api.terminals()) {
        if (session.owner === callerId) seen(next, session, terminal);
      }
      const listed = await api.listSessions({ owner: callerId });
      if (current !== generation) return;
      // 3. Events are newer than the listing: skip what they changed.
      for (const session of listed) {
        if (!changedByEvents.has(session.id)) seen(next, session);
      }
      changedByEvents = undefined;
    }
    // 4. Swap in the new state.
    perchEvents?.dispose();
    perchEvents = events;
    perch = api;
    tracked = next;
  } catch {
    // Perch failed: keep what we had.
  } finally {
    if (events !== perchEvents) events?.dispose();
  }
}

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.extensions.onDidChange(() => void connectPerch()),
    { dispose: () => { generation++; perchEvents?.dispose(); } },
  );
  void connectPerch();
}
```

## getApi

```ts
getApi(version: number, callerId: string): PerchApiV1 | undefined
```

The extension's `exports` has one member, `getApi`. Version 1 is the only one. Any other version returns `undefined`; keep your behavior without Perch then.

`callerId` is your extension's ID, such as `publisher.my-extension`. Perch records it as the `owner` of the sessions you create through that API object, so you can find them again with `listSessions({ owner })`. Each `callerId` gets one API object; calling `getApi` again returns the same one. An empty `callerId` creates sessions without an owner. VS Code does not tell Perch who called it, so ownership is a convention for finding your own sessions, not a security boundary.

## Types

```ts
interface SessionInfo {
  id: string;                  // 8 characters, [a-z0-9]
  name: string;
  kind: string;                // free text, such as 'shell'
  owner?: string;              // the callerId that created it
  annotations: Record<string, string>;
  dir: string;                 // the session directory, {dir}
  degraded: string[];          // problems the session has had, e.g. 'directory-recreated'
  cwd: string;
  host: string;
  createdAt: string;           // RFC 3339, UTC
  state: 'starting' | 'running' | 'exited' | 'lost' | 'unresponsive';
  attached: boolean;           // a terminal is attached, here or anywhere
  attachedHere: boolean;       // attached by a terminal of this window
  lastOutputAt?: string;
  memoryBytes?: number;        // resident memory of the program's terminal session, an estimate
  exit?: { code?: number; signal?: string };
}

interface CreateOptions {
  name: string;                // the session's and the terminal's name
  kind: string;
  command: string[];           // {dir} and {id} expand; {{ and }} are literal braces
  cwd?: string;
  env?: Record<string, string>; // the same in values
  annotations?: Record<string, string>;
  terminal?: {
    location?: vscode.TerminalLocation;
    iconPath?: vscode.TerminalOptions['iconPath'];
    color?: vscode.ThemeColor;
  };
}

interface PerchApiV1 {
  readonly version: 1;
  available(): Promise<boolean>;
  prepareLaunch(options: CreateOptions): { id: string; options: vscode.TerminalOptions };
  createSession(options: CreateOptions & { show?: boolean }): Promise<{ session: SessionInfo; terminal: vscode.Terminal }>;
  waitForSession(id: string, terminal: vscode.Terminal): Promise<SessionInfo>;
  listSessions(filter?: { owner?: string; kind?: string }): Promise<SessionInfo[]>;
  terminals(): ReadonlyArray<{ session: SessionInfo; terminal: vscode.Terminal }>;
  attach(id: string, options?: { show?: boolean }): Promise<vscode.Terminal>;
  sessionForTerminal(terminal: vscode.Terminal): SessionInfo | undefined;
  send(id: string, text: string, options?: { paste?: boolean; submit?: boolean }): Promise<'queued'>;
  kill(id: string, options?: { force?: boolean }): Promise<void>;
  annotate(id: string, set: Record<string, string>, unset?: string[]): Promise<void>;
  readonly onDidChangeSessions: vscode.Event<void>;
  readonly onDidAttach: vscode.Event<{ session: SessionInfo; terminal: vscode.Terminal }>;
  readonly onDidEndSession: vscode.Event<SessionInfo>;
}
```

The `SessionInfo` objects are copies. Changing them changes nothing in Perch.

## Members

### available

Resolves `true` on a macOS or Linux host where the `perch` binary runs (`perch version` succeeds), else `false`. On a Windows host, the calls that act on sessions (`prepareLaunch`, `createSession`, `waitForSession`, `attach`, `send`, `kill`, `annotate`) throw or reject with a `PerchApiError` whose code is `unavailable`, `listSessions` resolves `[]`, and `terminals` and `sessionForTerminal` find nothing.

### createSession

Starts a session in a new terminal and resolves once the program is running.

1. Perch picks the session ID and builds the launch, as `prepareLaunch` does.
2. It creates a terminal running `perch new --id <id> ... -- <command>`, with `isTransient: true`. The program starts inside that terminal, so it gets the terminal's environment: `TERM`, `LANG`, `terminal.integrated.env.*` and so on, plus `PERCH_SESSION_ID`, `PERCH_SESSION_DIR` and your `env`.
3. It shows the terminal unless `show` is `false`, and waits until `perch info` reports the session `running`.

`running` means the program was started (`exec` succeeded). It does not mean the program is ready for input; check that yourself, for example by waiting for a socket the program creates in the session directory.

It rejects with an `Error` if the terminal closes first, if the program cannot be started (the message says why, for example that the command was not found), or after 10 seconds. A program that starts and exits at once resolves with `state: 'exited'`.

The session uses the user's settings: `perch.linkedEnvironment` for linked variables and `terminal.integrated.scrollback` for history. The terminal's name is `name`, or the kind's display name if `name` is empty.

It rejects with a `TypeError` (`prepareLaunch` throws one) if `name` or `kind` is not a string or `command` is not a non-empty array of strings.

### prepareLaunch

Returns the session ID and the `TerminalOptions` for a new session without creating the terminal. Use it in your own `TerminalProfileProvider`:

```ts
vscode.window.registerTerminalProfileProvider('myext.console', {
  provideTerminalProfile: () => {
    const { id, options } = perch.prepareLaunch({ name: 'REPL', kind: 'myrepl', command: [...] });
    pendingIds.add(id);
    return new vscode.TerminalProfile(options);
  },
});
```

Perch recognizes the terminal when it opens, from its `shellPath` (a file named `perch`) and `shellArgs` (`new --id <id> ...`). Map a terminal to its session with `sessionForTerminal`, and wait for the program with `waitForSession`. Matching terminals by the order they were created is not needed.

### waitForSession

```ts
waitForSession(id: string, terminal: vscode.Terminal): Promise<SessionInfo>
```

For a terminal from `prepareLaunch`: resolves once the program is running, with the same rules and errors as `createSession`.

### listSessions

Lists every session on the host for this user, from every workspace and every caller, including ended ones (`exited`, `lost`). It refreshes the list first. The filter matches `owner` and `kind` exactly.

### terminals

The Perch terminals open in this window that have not closed, with their sessions. It is synchronous and includes terminals whose session is still starting; their `state` is `starting` until a listing shows them. Call it when you get the API, after subscribing to the events: after a window reload or an extension host restart, it is how you find the terminals that already exist.

### attach

Opens a terminal attached to a running session, or shows the one already open in this window, and returns it. If the session is attached in another window, Perch asks the user "myrepl: main is open in another window. Move it here?" and rejects with an `Error` if they decline. It rejects with `no_session` if the session has ended or does not exist. `show: false` creates the terminal without showing it.

### sessionForTerminal

The session of a Perch terminal in this window, or `undefined` for any other terminal.

### send

```ts
send(id: string, text: string, options?: { paste?: boolean; submit?: boolean }): Promise<'queued'>
```

Queues `text` (a string; anything else is a `TypeError`) as input to the session's program, as if typed, and resolves `'queued'` once the session has accepted it. The bytes are then written in order unless the session ends first. It works whether a terminal is attached in this window, in another window, or not at all.

- `submit: true` appends a carriage return, which is what Enter sends.
- `paste: true` wraps the text in bracketed-paste markers if the program has turned bracketed paste on, and sends it raw otherwise.
- Perch adds nothing. It does not answer prompts such as a pager's `--More--`, and does not change the program's settings.

`send` has terminal semantics. Whether the program reads the text as one line, several or a paste depends on the program, as with typing. A long line to a program reading in the terminal's line mode can be cut by the kernel's line limit. When you need to know that code ran, use a channel of your own, such as a socket the program listens on in the session directory.

It rejects with a `PerchApiError`:

| `code` | Meaning |
|--------|---------|
| `queue_full` | Nothing was queued. The session's 1 MiB input queue had no room, or the text is over 1 MiB. |
| `outcome_unknown` | The connection ended, or the call timed out, before the answer. The text may have been queued. Do not send it again; tell the user it may not have run. |
| `no_session` | No such session, or it has ended. Nothing was sent. |
| `unavailable` | Perch cannot run here, or could not reach the session. Nothing was sent. |

### kill

Ends the session's program and everything it started in its terminal: SIGHUP, then SIGKILL after 5 seconds, or SIGKILL at once with `force: true`. It does not ask the user. It resolves once the session has ended, and leaves no entry behind. It rejects with `no_session` if the session does not exist or has already ended.

### annotate

Sets and removes annotations on a running session, then refreshes the list. An empty `set` and `unset` does nothing. Every extension can read annotations through `listSessions`, so keep secrets out of them; use files in the session directory (mode 0600) instead.

### Events

| Event | Fires |
|-------|-------|
| `onDidChangeSessions` | When a listing differs from the previous one: a session added or removed, or its state, attached client, name, working directory, memory, `degraded` list or annotations changed. Also after the first listing and when listing starts or stops failing. |
| `onDidAttach` | When a terminal of this window is attached to a session: after `createSession`, `attach`, a profile terminal from `prepareLaunch`, Perch's own open and restore, and adoption of an existing terminal at activation. It fires once the session reports this terminal's client as attached, and does not fire if that does not happen within 10 seconds. |
| `onDidEndSession` | Once per session, when Perch sees it end: it exited, was killed, or was lost. |

How a window learns about an end: a session that exits while detached stays listed as `exited` with its exit status. A session that exits while attached leaves no entry. The window whose terminal received the exit reports its exit code; another window sees the session disappear and reports it with `state: 'exited'` and no `exit`. The list refreshes every 30 seconds even while the Sessions view is hidden, so an open window notices an end within about 30 seconds.

### Errors

`PerchApiError` has a `name` of `'PerchApiError'` and a `code` from the table above. Check `err.code`, not `instanceof`, because the class is not exported. Calls other than `send` reject with a `PerchApiError` when one of the codes fits (`attach`, `kill` and `annotate` use `no_session` for a session that does not exist or has ended, and the calls that act on sessions use `unavailable` when Perch cannot run here), and otherwise with the underlying `Error`.

## Conventions

**Owner.** Pass your extension ID as `callerId`. Find your sessions with `listSessions({ owner: callerId })`. Sessions created by the user (Perch's own terminals) have no owner.

**Kind.** A short lower-case word for the program, such as `python` or `node`. The Sessions view shows it as given, with a generic icon. Perch's own terminals use `shell`, shown as Shell with a terminal icon.

**Annotations.** Use keys prefixed with your project's name, such as `myext.socket`. Values are strings. They are for marking sessions, not for secrets or data that changes often; each change is a request to the session's supervisor.

**The session directory.** `{dir}` in `command` and `env` is the session directory, mode 0700, which exists before the program starts. Programs and extensions may keep files there, such as sockets and descriptor files. Paths there are short (`/tmp/perch-1000/k3x9q2ab/ipc`), well within the socket path limit. It is removed when the session ends. Write files there atomically (write a temporary file, then rename it), with mode 0600 for anything secret. If `degraded` includes `directory-recreated`, the directory was removed and recreated while the session ran, and files kept there, such as a socket your program listened on, are gone; fall back to `send`.

**Literal braces.** `{dir}` and `{id}` expand in `command` and in `env` values, `{{` and `}}` are literal braces, and any other `{` or `}` is an error. Escape text that comes from the user or the file system (paths, arguments, command lines) by doubling its braces, such as `s.replace(/[{}]/g, (c) => c + c)`.

**Choosing where to send.** `listSessions` returns sessions from every workspace and window on the host. Sessions with a terminal in this window (`terminals()`, `onDidAttach`) are the ones the user is working with here. Send to another session only when the user picks it.

**Sending.** Use the program's own channel when it has one. Otherwise use `send`, not `terminal.sendText`: `sendText` reaches nothing while the session is detached.

**Closing a terminal is a detach.** The program keeps running. Do not clean up when a Perch terminal closes; clean up on `onDidEndSession`.

**No startup input.** Pass everything the program needs on its command line or in `env`. Do not `send` setup code at start; a program that is still starting can lose it.

**Programs that wait.** A program that pauses for a key, such as a pager or a `--More--` prompt, waits there while detached until someone attaches, because Perch never answers it. Programs that hold a resource, such as a license seat, keep holding it while detached.
