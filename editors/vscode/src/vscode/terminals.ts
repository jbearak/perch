// SPDX-License-Identifier: GPL-3.0-only

// Perch terminals: creating sessions in terminals, opening and restoring
// them, recognizing and adopting the ones already open, and what a closed
// terminal means. The extension never types into a session; a terminal's
// process is `perch new` or `perch attach`.
import * as vscode from 'vscode';
import { PerchCli } from '../core/cli';
import { attachArgs, newArgs, newSessionId, NewSession, recognize, Recognized, scrollbackLines, shellCommand } from '../core/launch';
import { waitUntilRunning } from '../core/readiness';
import {
  adopt,
  closeAction,
  openStep,
  ExitReason,
  RestoreSet,
  RestoreStore,
  Restorer,
  settle75,
  TerminalRegistry,
} from '../core/restore';
import { displayName, iconName, kindIcon, kindName, Session } from '../core/sessions';
import { Settings } from '../core/settings';
import type { Confirm } from './activation';

export interface CreateOptions {
  name: string;
  kind: string;
  command: string[];
  cwd?: string;
  env?: Record<string, string>;
  annotations?: Record<string, string>;
  owner?: string;
  terminal?: {
    location?: vscode.TerminalLocation;
    iconPath?: vscode.TerminalOptions['iconPath'];
    color?: vscode.ThemeColor;
  };
}

/** What happened when a Perch terminal closed, for the tests. */
export interface CloseOutcome {
  id: string;
  reason: ExitReason;
  code: number | undefined;
  /** `forget`, `keep`, `ignore`, or for code 75 `busy` or `ended`. */
  result: string;
}

/** Shows the first-close notice; resolves with the button chosen. */
export type Notify = (message: string, ...items: string[]) => Thenable<string | undefined>;

const restoreKey = 'perch.restoreSet';
const noticeKey = 'perch.closeNotice.hidden';

export interface TerminalsDeps {
  context: vscode.ExtensionContext;
  output: vscode.OutputChannel;
  label: string;
  /** Whether this window has a workspace: empty windows restore nothing. */
  hasWorkspace: boolean;
  cli(): PerchCli;
  settings(): Settings;
  confirm(): Confirm;
  /** The latest listing's view of a session. */
  find(id: string): Session | undefined;
  /**
   * Where the restore set lives instead of `workspaceState`: only the
   * cross-process test sets it, because VS Code keeps workspace storage in
   * memory during extension tests.
   */
  restoreStore?: RestoreStore;
  /** Refreshes the session list after an action. */
  refresh(): Promise<void>;
}

export class Terminals implements vscode.Disposable {
  readonly registry = new TerminalRegistry<vscode.Terminal>();
  readonly restoreSet: RestoreSet;
  readonly restorer: Restorer;
  readonly closeOutcomes: CloseOutcome[] = [];
  notify: Notify = (message, ...items) => vscode.window.showInformationMessage(message, ...items);
  private readonly disposables: vscode.Disposable[] = [];
  /** Terminals this extension is closing itself. */
  private readonly closing = new WeakSet<vscode.Terminal>();
  /** Open's guarded attaches, which ask before a takeover if they find the session busy. */
  private readonly opening = new WeakSet<vscode.Terminal>();
  private noticeShown = false;
  private linkEnv: Promise<boolean> | undefined;
  private linkEnvWarned = false;
  private readonly closedEmitter = new vscode.EventEmitter<CloseOutcome>();
  readonly onDidClose = this.closedEmitter.event;

  constructor(private readonly deps: TerminalsDeps) {
    const state = deps.context.workspaceState;
    this.restoreSet = new RestoreSet(
      !deps.hasWorkspace
        ? { get: () => [], set: async () => undefined }
        : (deps.restoreStore ?? {
            get: () => state.get<string[]>(restoreKey, []),
            set: (ids) => state.update(restoreKey, ids),
          }),
    );
    this.restorer = new Restorer({
      set: this.restoreSet,
      label: deps.label,
      info: (id) => deps.cli().info(id),
      hasTerminal: (id) => this.registry.terminalFor(id) !== undefined,
      open: (s) => {
        this.output(`Restoring ${displayName(s)} (${s.id})`);
        this.attachTerminal(s, { ifDetached: true, preserveFocus: true });
      },
      now: () => Date.now(),
      log: (m) => this.output(m),
    });
    this.disposables.push(
      this.closedEmitter,
      vscode.window.onDidOpenTerminal((t) => this.opened(t)),
      vscode.window.onDidCloseTerminal((t) => void this.closed(t)),
    );
  }

  /** At activation: adopt the Perch terminals already open, then restore. */
  async start(): Promise<void> {
    const adopted = adopt(this.registry, vscode.window.terminals, recognizeTerminal);
    for (const { launch } of adopted) {
      this.output(`Adopted the terminal of ${launch.id}`);
      await this.restoreSet.add(launch.id);
    }
    if (this.deps.hasWorkspace && this.deps.settings().restoreTerminals) await this.restorer.start();
  }

  /** On each list refresh: retry pending restores while the window is open. */
  retryRestores(): Promise<void> {
    return this.restorer.pass();
  }

  private opened(t: vscode.Terminal): void {
    const launch = recognizeTerminal(t);
    if (launch === undefined || !this.registry.track(t, launch)) return;
    void this.restoreSet.add(launch.id);
  }

  private async closed(t: vscode.Terminal): Promise<void> {
    const launch = this.registry.untrack(t);
    if (launch === undefined) return;
    const reason = exitReason(t);
    const code = t.exitStatus?.code;
    const action = this.closing.has(t) ? 'ignore' : closeAction(reason, code);
    let result: string = action;
    try {
      if (action === 'forget') {
        await this.restoreSet.remove(launch.id);
        if (reason === 'user') await this.firstCloseNotice(launch.id);
      } else if (action === 'confirm') {
        // A guarded Open that found the session busy asks again; a restore
        // retries while its window is open.
        const opening = this.opening.has(t);
        const retry = opening ? (id: string) => void this.reopen(id) : (id: string) => this.restorer.busy(id);
        result = await settle75(launch.id, () => this.deps.cli().info(launch.id), this.restoreSet, retry);
        if (result === 'busy') {
          // Another client got there first: close the terminal quietly.
          this.output(`${launch.id} is attached elsewhere; leaving it there`);
          this.closing.add(t);
          t.dispose();
        }
      }
    } finally {
      const outcome = { id: launch.id, reason, code, result };
      this.closeOutcomes.push(outcome);
      this.closedEmitter.fire(outcome);
      void this.deps.refresh();
    }
  }

  /** Closing a terminal detaches; the first time, say so. */
  private async firstCloseNotice(id: string): Promise<void> {
    if (this.noticeShown || this.deps.context.globalState.get<boolean>(noticeKey, false)) return;
    const info = await this.deps.cli().info(id).catch(() => undefined);
    if (info === undefined || info.state !== 'running') return;
    this.noticeShown = true;
    const kill = 'Kill';
    const hide = "Don't Show Again";
    void this.notify(
      `${displayName(info)} is still running. Closing a Perch terminal detaches it; open it again from the Perch view.`,
      kill,
      hide,
    ).then(async (choice) => {
      if (choice === hide) await this.deps.context.globalState.update(noticeKey, true);
      if (choice === kill) await vscode.commands.executeCommand('perch.kill', id);
    });
  }

  /** The launch for a new session, without creating a terminal. */
  async prepareLaunch(o: CreateOptions): Promise<{ id: string; options: vscode.TerminalOptions }> {
    const id = newSessionId();
    const settings = this.deps.settings();
    const spec: NewSession = {
      id,
      label: this.deps.label,
      scrollback: scrollback(),
      name: o.name,
      kind: o.kind,
      command: o.command,
      owner: o.owner,
      cwd: o.cwd,
      env: o.env,
      annotations: o.annotations,
      linkEnv: (await this.linkEnvSupported(settings.linkedEnvironment)) ? settings.linkedEnvironment : [],
    };
    return {
      id,
      options: {
        name: o.name !== '' ? o.name : kindName(o.kind),
        shellPath: this.deps.cli().binary,
        shellArgs: newArgs(spec),
        cwd: o.cwd,
        isTransient: true,
        iconPath: o.terminal?.iconPath ?? new vscode.ThemeIcon(kindIcon(o.kind)),
        color: o.terminal?.color,
        location: o.terminal?.location,
      },
    };
  }

  /**
   * `--link-env` only when `perch.linkedEnvironment` names something, and
   * only if this binary has the flag; otherwise say once that it is ignored.
   */
  private async linkEnvSupported(names: readonly string[]): Promise<boolean> {
    if (names.length === 0) return false;
    this.linkEnv ??= this.deps.cli().supportsLinkEnv();
    const ok = await this.linkEnv;
    if (!ok && !this.linkEnvWarned) {
      this.linkEnvWarned = true;
      const message = 'This version of perch does not support perch.linkedEnvironment yet; sessions start without linked variables.';
      this.output(message);
      void vscode.window.showWarningMessage(message);
    }
    return ok;
  }

  /** The binary or the setting changed: probe `--link-env` again. */
  binaryChanged(): void {
    this.linkEnv = undefined;
    this.linkEnvWarned = false;
  }

  /** Creates the terminal and resolves once the program is running. */
  async createSession(o: CreateOptions & { show?: boolean }): Promise<{ session: Session; terminal: vscode.Terminal }> {
    const { id, options } = await this.prepareLaunch(o);
    const terminal = vscode.window.createTerminal(options);
    this.registry.track(terminal, { id, mode: 'new', ifDetached: false });
    await this.restoreSet.add(id);
    if (o.show !== false) terminal.show();
    const session = await this.waitForSession(id, terminal, options.name ?? id);
    void this.deps.refresh();
    return { session, terminal };
  }

  /** Resolves once a terminal from prepareLaunch has a running session. */
  waitForSession(id: string, terminal: vscode.Terminal, name: string): Promise<Session> {
    return waitUntilRunning(name, {
      info: () => this.deps.cli().info(id),
      closed: () => (terminal.exitStatus === undefined ? undefined : (terminal.exitStatus.code ?? null)),
      now: () => Date.now(),
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
    });
  }

  /** The persistent shell's options. */
  shellOptions(): CreateOptions {
    const command = shellCommand(this.deps.settings().shell, process.platform, process.env.SHELL);
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri;
    return {
      name: 'Persistent shell',
      kind: 'shell',
      command,
      cwd: cwd?.scheme === 'file' ? cwd.fsPath : undefined,
    };
  }

  /** "Perch: New Persistent Shell". */
  async newShell(): Promise<void> {
    await this.createSession({ ...this.shellOptions(), show: true });
  }

  /** The "Persistent shell" terminal profile. */
  profileProvider(): vscode.TerminalProfileProvider {
    return {
      provideTerminalProfile: async () => {
        const { id, options } = await this.prepareLaunch(this.shellOptions());
        // The terminal appears through onDidOpenTerminal; report a failed start.
        void this.watchProfileTerminal(id, options.name ?? id);
        return new vscode.TerminalProfile(options);
      },
    };
  }

  private async watchProfileTerminal(id: string, name: string): Promise<void> {
    const end = Date.now() + 10_000;
    let terminal: vscode.Terminal | undefined;
    while (terminal === undefined && Date.now() < end) {
      terminal = this.registry.terminalFor(id);
      if (terminal === undefined) await new Promise((r) => setTimeout(r, 100));
    }
    if (terminal === undefined) return; // The profile was not used.
    try {
      await this.waitForSession(id, terminal, name);
    } catch (err) {
      void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
    }
    void this.deps.refresh();
  }

  /**
   * Opens (or shows) a terminal attached to the session. If another client
   * is attached, asks first: opening moves it here. Until the user agrees
   * to a takeover the attach is guarded by `--if-detached`.
   */
  async open(s: Session): Promise<vscode.Terminal | undefined> {
    const existing = this.registry.terminalFor(s.id);
    if (existing !== undefined) {
      existing.show();
      return existing;
    }
    return this.openWith((await this.deps.cli().info(s.id)) ?? s);
  }

  /** A guarded Open found the session busy: ask, as Open does. */
  private async reopen(id: string): Promise<void> {
    try {
      const info = await this.deps.cli().info(id);
      if (info !== undefined && this.registry.terminalFor(id) === undefined) await this.openWith(info);
    } catch (err) {
      void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
    }
  }

  /** Open from a session as last checked; the tests pass a stale one to recreate the race. */
  async openWith(info: Session): Promise<vscode.Terminal | undefined> {
    let step = openStep(info, false);
    if (step === 'not-running') {
      throw new Error(`${displayName(info)} is ${info.state === 'exited' ? 'no longer running' : info.state}.`);
    }
    if (step === 'confirm') {
      const question = `${kindName(info.kind)}: ${displayName(info)} is open in another window. Move it here?`;
      const detail = 'The other window keeps its terminal, which shows that the session moved.';
      if (!(await this.deps.confirm()(question, detail, 'Move Here'))) return undefined;
      step = openStep(info, true);
    }
    const terminal = this.attachTerminal(info, { ifDetached: step === 'attach-guarded', preserveFocus: false });
    if (step === 'attach-guarded') this.opening.add(terminal);
    void this.deps.refresh();
    return terminal;
  }

  /** A terminal running `perch attach`, tracked and in the restore set. */
  attachTerminal(s: Session, o: { ifDetached: boolean; preserveFocus: boolean }): vscode.Terminal {
    const terminal = vscode.window.createTerminal({
      name: displayName(s),
      shellPath: this.deps.cli().binary,
      shellArgs: attachArgs(s.id, this.deps.label, scrollback(), o.ifDetached),
      isTransient: true,
      iconPath: new vscode.ThemeIcon(iconName(s)),
    });
    this.registry.track(terminal, { id: s.id, mode: 'attach', ifDetached: o.ifDetached });
    void this.restoreSet.add(s.id);
    terminal.show(o.preserveFocus);
    return terminal;
  }

  terminalFor(id: string): vscode.Terminal | undefined {
    return this.registry.terminalFor(id);
  }

  private output(message: string): void {
    this.deps.output.appendLine(message);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}

/** A Perch terminal from its creation options, unless it has already exited. */
export function recognizeTerminal(t: vscode.Terminal): Recognized | undefined {
  if (t.exitStatus !== undefined) return undefined;
  const o = t.creationOptions as Readonly<vscode.TerminalOptions>;
  return recognize(o.shellPath, o.shellArgs);
}

function exitReason(t: vscode.Terminal): ExitReason {
  switch (t.exitStatus?.reason) {
    case vscode.TerminalExitReason.Shutdown:
      return 'shutdown';
    case vscode.TerminalExitReason.Process:
      return 'process';
    case vscode.TerminalExitReason.User:
      return 'user';
    case vscode.TerminalExitReason.Extension:
      return 'extension';
    default:
      return 'unknown';
  }
}

/** `terminal.integrated.scrollback`, clamped for `--scrollback`. */
function scrollback(): number {
  return scrollbackLines(vscode.workspace.getConfiguration('terminal.integrated').get('scrollback'));
}
