// SPDX-License-Identifier: GPL-3.0-only

// The VS Code side of activation: the binary, the session list, kill and
// dismiss, the status bar count, the command-line copy, terminals, the
// `perch doctor` warning and the API for other extensions.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { available, commandLineDir, ensureExecutable, hostSupported, resolveBinary } from '../core/binary';
import { runtimeDir } from '../core/api';
import { PerchCli } from '../core/cli';
import { doctorWarnedKey, doctorWarning, parseDoctor } from '../core/doctor';
import { clientLabel, windowKey, workspaceKey } from '../core/labels';
import { RefreshScheduler } from '../core/refresh';
import { RestoreStore } from '../core/restore';
import {
  describe,
  DescribeContext,
  displayName,
  exitText,
  isDetached,
  killConfirmation,
  Session,
  stateText,
  stripAnsi,
} from '../core/sessions';
import { readSettings, Settings } from '../core/settings';
import { Api, PerchApiV1 } from './api';
import { CommandLine } from './commandLine';
import { DetachedCount, SessionItem, SessionsProvider, SessionStore } from './sessionsView';
import { CloseOutcome, CreateOptions, Notify, Terminals } from './terminals';

/** Asks the user to confirm; resolves true to go ahead. */
export type Confirm = (message: string, detail: string, action: string) => Promise<boolean>;

const modalConfirm: Confirm = async (message, detail, action) =>
  (await vscode.window.showWarningMessage(message, { modal: true, detail }, action)) === action;

/** Seams for the integration tests, exported only in test mode. */
export interface Testing {
  label: string;
  sessions(): readonly Session[];
  refresh(): Promise<void>;
  setConfirm(confirm: Confirm | undefined): void;
  available(platform?: NodeJS.Platform): Promise<boolean>;
  detachedCount(): number;
  /** Settles when the activation's command-line install has finished. */
  commandLineInstalled: Promise<void>;
  commandLineDir: string;
  environment: vscode.EnvironmentVariableCollection;
  /** Settles when adoption and the first restore pass have run. */
  terminalsStarted: Promise<void>;
  createSession(o: CreateOptions): Promise<{ session: Session; terminal: vscode.Terminal }>;
  prepareLaunch(o: CreateOptions): { id: string; options: vscode.TerminalOptions };
  /** The Perch terminal attached to a session in this window. */
  terminalFor(id: string): vscode.Terminal | undefined;
  restoreSet(): string[];
  addToRestoreSet(id: string): Promise<void>;
  /** A restore pass as activation runs it, with a fresh 60-second window. */
  restore(): Promise<void>;
  pendingRestores(): string[];
  /** Opens a restore terminal (`--if-detached`) whatever the session's state. */
  restoreAttach(id: string): vscode.Terminal;
  /** Open from the last listing, without checking again: the race with another window. */
  openStale(id: string): Promise<vscode.Terminal | undefined>;
  closeOutcomes(): readonly CloseOutcome[];
  onDidClose: vscode.Event<CloseOutcome>;
  setNotify(notify: Notify | undefined): void;
  shellCommand(): string[];
  /** Settles with the warning `perch doctor` led to, if any, once it has run. */
  doctorChecked: Promise<string | undefined>;
}

export interface Exports {
  /** The API for other extensions; undefined for a version this Perch does not serve. */
  getApi(version: number, callerId: string): PerchApiV1 | undefined;
  testing?: Testing;
}

const unsupportedMessage =
  'Perch needs a macOS or Linux host. Open a remote window (Remote-SSH, WSL or Dev Containers) to use it.';

export function activate(context: vscode.ExtensionContext): Exports {
  const output = vscode.window.createOutputChannel('Perch');
  context.subscriptions.push(output);
  const platform = process.platform;
  const supported = hostSupported(platform);
  const settings = (): Settings => {
    const config = vscode.workspace.getConfiguration('perch');
    return readSettings((key) => config.get(key));
  };
  const label = clientLabel(workspaceKey(workspaceUri()), windowKey());
  const home = os.homedir();
  const describeContext = (): DescribeContext => ({ now: Date.now(), home, label });

  const binaryFor = (s: Settings) =>
    resolveBinary({
      configuredPath: s.path,
      extensionPath: context.extensionPath,
      platform,
      arch: process.arch,
      home,
    });
  let cli = new PerchCli(binaryFor(settings()).path);
  if (supported) makeBundledExecutable(context.extensionPath, output);

  const store = new SessionStore();
  const provider = new SessionsProvider(store, describeContext);
  const view = vscode.window.createTreeView('perch.sessions', { treeDataProvider: provider });
  view.title = `Sessions on ${os.hostname()}`;
  const count = new DetachedCount(store);
  context.subscriptions.push(store, provider, view, count);

  const refresh = async (): Promise<void> => {
    if (!supported) {
      store.set([], unsupportedMessage);
      return;
    }
    try {
      const startedAt = performance.now();
      store.set(await cli.list(), undefined, startedAt);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message !== store.error) output.appendLine(`Listing sessions failed: ${message}`);
      store.set([...store.sessions], message);
    }
  };
  context.subscriptions.push(
    store.onDidChange(() => {
      view.message = store.error ?? (store.sessions.length === 0 ? 'No sessions on this host.' : undefined);
    }),
  );
  const scheduler = new RefreshScheduler(refresh);
  context.subscriptions.push(
    { dispose: () => scheduler.dispose() },
    view.onDidChangeVisibility((e) => scheduler.setVisible(e.visible)),
    vscode.window.onDidChangeWindowState((e) => {
      if (e.focused) scheduler.focused();
    }),
  );
  scheduler.start(view.visible);

  let confirm: Confirm = modalConfirm;
  const terminals = new Terminals({
    context,
    output,
    label,
    hasWorkspace: workspaceUri() !== undefined,
    cli: () => cli,
    settings,
    confirm: () => confirm,
    find: (id) => store.find(id),
    refresh: () => scheduler.now(),
    restoreStore: testRestoreStore(context),
  });
  context.subscriptions.push(terminals);
  const terminalsStarted = supported
    ? terminals.start().catch((err) => output.appendLine(`Restoring terminals failed: ${String(err)}`))
    : Promise.resolve();
  context.subscriptions.push(
    store.onDidRefresh(() => {
      void terminalsStarted.then(() => terminals.retryRestores());
    }),
  );
  if (supported) {
    context.subscriptions.push(
      vscode.window.registerTerminalProfileProvider('perch.persistentShell', terminals.profileProvider()),
    );
  }

  const api = new Api({
    supported,
    cli: () => cli,
    find: (id) => store.find(id),
    sessions: () => store.sessions,
    refresh: () => scheduler.now(),
    terminals,
    runtimeDir: runtimeDir(process.env, process.getuid?.() ?? 0, process.cwd()),
    onDidChangeList: store.onDidChange,
    onDidList: store.onDidList,
  });
  context.subscriptions.push(api);
  const getApi = (version: number, callerId: string) => api.getApi(version, callerId);

  const doctorChecked = supported ? checkHost(context, cli, output) : Promise.resolve(undefined);

  const commandLine = new CommandLine(
    commandLineDir(context.globalStorageUri.fsPath, platform, process.arch),
    context.environmentVariableCollection,
    output,
  );
  const commandLineInstalled = supported ? commandLine.install(cli, settings().addToPath) : Promise.resolve();

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('perch.linkedEnvironment')) terminals.binaryChanged();
      if (e.affectsConfiguration('perch.path')) {
        cli = new PerchCli(binaryFor(settings()).path);
        terminals.binaryChanged();
        void scheduler.now();
      }
      if (e.affectsConfiguration('perch.addToPath') && supported) commandLine.apply(settings().addToPath);
    }),
  );

  const sessionFrom = (arg: unknown): Session | undefined => {
    if (arg instanceof SessionItem) return store.find(arg.session.id) ?? arg.session;
    if (typeof arg === 'string') return store.find(arg);
    return undefined;
  };
  const guard = (fn: (arg: unknown) => Promise<void>) => async (arg: unknown) => {
    if (!supported) {
      void vscode.window.showErrorMessage(unsupportedMessage);
      return;
    }
    try {
      await fn(arg);
    } catch (err) {
      void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
    }
  };

  const kill = async (arg: unknown): Promise<void> => {
    const s = sessionFrom(arg);
    if (s === undefined) return;
    const { message, detail } = killConfirmation(s, describeContext());
    if (!(await confirm(message, detail, 'Kill'))) return;
    try {
      await cli.kill(s.id);
    } finally {
      await scheduler.now();
    }
  };

  const dismiss = async (arg: unknown): Promise<void> => {
    const s = sessionFrom(arg);
    if (s === undefined) return;
    try {
      await cli.dismiss(s.id);
    } finally {
      await scheduler.now();
    }
  };

  const showLastOutput = async (arg: unknown): Promise<void> => {
    const s = sessionFrom(arg);
    if (s === undefined) return;
    const doc = await vscode.workspace.openTextDocument({ content: lastOutput(s, label), language: 'plaintext' });
    await vscode.window.showTextDocument(doc, { preview: true });
  };

  const open = async (arg: unknown): Promise<void> => {
    let s = sessionFrom(arg);
    if (s === undefined && arg === undefined) s = await pickLive();
    if (s === undefined) return;
    await terminals.open(s);
  };

  const pickLive = async (): Promise<Session | undefined> => {
    await scheduler.now();
    const ctx = describeContext();
    const live = store.sessions.filter((s) => s.state === 'running');
    if (live.length === 0) {
      void vscode.window.showInformationMessage('No running Perch sessions.');
      return undefined;
    }
    const chosen = await vscode.window.showQuickPick(
      live.map((s) => ({ id: s.id, label: displayName(s), description: describe(s, ctx) })),
      { title: `Open a session on ${os.hostname()}`, placeHolder: 'Select a session to open' },
    );
    return chosen && store.find(chosen.id);
  };

  const showDetached = async (): Promise<void> => {
    await scheduler.now();
    const ctx = describeContext();
    const detached = store.sessions.filter(isDetached);
    if (detached.length === 0) {
      void vscode.window.showInformationMessage('No detached Perch sessions.');
      return;
    }
    const killButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon('trash'), tooltip: 'Kill' };
    const revealButton: vscode.QuickInputButton = {
      iconPath: new vscode.ThemeIcon('list-tree'),
      tooltip: 'Show in the Perch View',
    };
    const pick = vscode.window.createQuickPick<vscode.QuickPickItem & { id: string }>();
    pick.title = `Detached sessions on ${os.hostname()}`;
    pick.placeholder = 'Select a session to open it';
    pick.items = detached.map((s) => ({
      id: s.id,
      label: displayName(s),
      description: describe(s, ctx),
      buttons: [revealButton, killButton],
    }));
    pick.onDidTriggerItemButton((e) => {
      pick.hide();
      if (e.button === killButton) {
        void guard(kill)(e.item.id);
        return;
      }
      const item = provider.item(e.item.id);
      if (item !== undefined) void view.reveal(item, { select: true, focus: true });
    });
    pick.onDidAccept(() => {
      const chosen = pick.selectedItems[0];
      pick.hide();
      if (chosen !== undefined) void guard(open)(chosen.id);
    });
    pick.onDidHide(() => pick.dispose());
    pick.show();
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('perch.refresh', () => scheduler.now()),
    vscode.commands.registerCommand('perch.open', guard(open)),
    vscode.commands.registerCommand('perch.newShell', guard(() => terminals.newShell())),
    vscode.commands.registerCommand('perch.kill', guard(kill)),
    vscode.commands.registerCommand('perch.dismiss', guard(dismiss)),
    vscode.commands.registerCommand('perch.showLastOutput', guard(showLastOutput)),
    vscode.commands.registerCommand('perch.showDetached', guard(showDetached)),
    vscode.commands.registerCommand('perch.showCommandLinePath', () =>
      supported ? commandLine.show() : vscode.window.showErrorMessage(unsupportedMessage),
    ),
  );

  if (context.extensionMode !== vscode.ExtensionMode.Test) return { getApi };
  return {
    getApi,
    testing: {
      label,
      sessions: () => store.sessions,
      refresh: () => scheduler.now(),
      setConfirm: (c) => {
        confirm = c ?? modalConfirm;
      },
      available: (p = platform) => available(p, () => cli.works()),
      detachedCount: () => count.count,
      commandLineInstalled,
      commandLineDir: commandLine.dir,
      environment: context.environmentVariableCollection,
      terminalsStarted,
      createSession: (o) => terminals.createSession(o),
      prepareLaunch: (o) => terminals.prepareLaunch(o),
      terminalFor: (id) => terminals.terminalFor(id),
      restoreSet: () => terminals.restoreSet.ids(),
      addToRestoreSet: (id) => terminals.restoreSet.add(id),
      restore: () => terminals.restorer.start(),
      pendingRestores: () => terminals.restorer.pendingIds(),
      restoreAttach: (id) => {
        const s = store.find(id);
        if (s === undefined) throw new Error(`${id} is not listed`);
        return terminals.attachTerminal(s, { ifDetached: true, preserveFocus: true });
      },
      openStale: (id) => {
        const s = store.find(id);
        if (s === undefined) throw new Error(`${id} is not listed`);
        return terminals.openWith(s);
      },
      closeOutcomes: () => terminals.closeOutcomes,
      onDidClose: terminals.onDidClose,
      setNotify: (n) => {
        terminals.notify = n ?? ((m, ...items) => vscode.window.showInformationMessage(m, ...items));
      },
      shellCommand: () => terminals.shellOptions().command,
      doctorChecked,
    },
  };
}

export function deactivate(): void {}

/**
 * Runs `perch doctor` and, once per host, warns when sessions there are not
 * expected to survive logout. Resolves with the warning, if any.
 */
async function checkHost(
  context: vscode.ExtensionContext,
  cli: PerchCli,
  output: vscode.OutputChannel,
): Promise<string | undefined> {
  const host = os.hostname();
  try {
    const report = parseDoctor(await cli.doctor());
    for (const c of report.checks) output.appendLine(`perch doctor: ${c.status} ${c.name}: ${c.message}`);
    const warning = doctorWarning(report, host);
    if (warning === undefined) return undefined;
    const key = doctorWarnedKey(host);
    if (!context.globalState.get<boolean>(key, false)) {
      await context.globalState.update(key, true);
      void vscode.window.showWarningMessage(warning);
    }
    return warning;
  } catch (err) {
    output.appendLine(`perch doctor failed: ${err instanceof Error ? err.message : String(err)}`);
    return undefined;
  }
}

/**
 * In test mode with PERCH_TEST_RESTORE_FILE set, the restore set lives in
 * that file. VS Code keeps workspace storage in memory during extension
 * tests, so this is how the cross-process test carries it from one launch
 * to the next.
 */
function testRestoreStore(context: vscode.ExtensionContext): RestoreStore | undefined {
  const file = process.env.PERCH_TEST_RESTORE_FILE;
  if (context.extensionMode !== vscode.ExtensionMode.Test || file === undefined || file === '') return undefined;
  return {
    get: () => {
      try {
        return JSON.parse(fs.readFileSync(file, 'utf8')) as string[];
      } catch {
        return [];
      }
    },
    set: async (ids) => fs.writeFileSync(file, JSON.stringify(ids)),
  };
}

/** The URI that identifies this workspace, or undefined in an empty window. */
function workspaceUri(): string | undefined {
  const file = vscode.workspace.workspaceFile;
  if (file !== undefined && file.scheme !== 'untitled') return file.toString();
  return vscode.workspace.workspaceFolders?.[0]?.uri.toString();
}

function makeBundledExecutable(extensionPath: string, output: vscode.OutputChannel): void {
  const bundled = resolveBinary({ configuredPath: '', extensionPath, platform: process.platform, arch: process.arch, home: '' });
  try {
    if (ensureExecutable(bundled.path)) output.appendLine(`Made ${bundled.path} executable`);
  } catch (err) {
    // No bundled binary (a universal package, or a development checkout).
    output.appendLine(`No bundled binary: ${String(err)}`);
  }
}

/** The document "Show last output" opens. */
function lastOutput(s: Session, label: string): string {
  const header = `${displayName(s)} (${s.id}): ${stateText(s, label)}`;
  if (s.state === 'lost') {
    return `${header}\n\nIts supervisor ended unexpectedly, so no output was saved. ` +
      `${path.join(s.dir, 'supervisor.log')} may say why.\n`;
  }
  let tail = '';
  try {
    tail = stripAnsi(fs.readFileSync(path.join(s.dir, 'tail.txt'), 'utf8'));
  } catch {
    // Missing: nothing was saved.
  }
  const status = s.state === 'exited' ? `It ended with ${exitText(s.exit)}.` : '';
  return `${header}\n${status}\n\n${tail.trim() !== '' ? tail : '(No output was saved.)\n'}`;
}
