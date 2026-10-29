// SPDX-License-Identifier: GPL-3.0-only

// The VS Code side of activation: the binary, the session list, kill and
// dismiss, the status bar count and the command-line copy. Terminals and
// the API arrive in later milestones.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { available, commandLineDir, ensureExecutable, hostSupported, resolveBinary } from '../core/binary';
import { PerchCli } from '../core/cli';
import { clientLabel, windowKey, workspaceKey } from '../core/labels';
import { RefreshScheduler } from '../core/refresh';
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
import { CommandLine } from './commandLine';
import { DetachedCount, SessionItem, SessionsProvider, SessionStore } from './sessionsView';

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
}

export interface Exports {
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
      store.set(await cli.list());
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

  const commandLine = new CommandLine(
    commandLineDir(context.globalStorageUri.fsPath, platform, process.arch),
    context.environmentVariableCollection,
    output,
  );
  const commandLineInstalled = supported ? commandLine.install(cli, settings().addToPath) : Promise.resolve();

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('perch.path')) {
        cli = new PerchCli(binaryFor(settings()).path);
        void scheduler.now();
      }
      if (e.affectsConfiguration('perch.addToPath') && supported) commandLine.apply(settings().addToPath);
    }),
  );

  let confirm: Confirm = modalConfirm;
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

  const showDetached = async (): Promise<void> => {
    await scheduler.now();
    const ctx = describeContext();
    const detached = store.sessions.filter(isDetached);
    if (detached.length === 0) {
      void vscode.window.showInformationMessage('No detached Perch sessions.');
      return;
    }
    const killButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon('trash'), tooltip: 'Kill' };
    const pick = vscode.window.createQuickPick<vscode.QuickPickItem & { id: string }>();
    pick.title = `Detached sessions on ${os.hostname()}`;
    pick.placeholder = 'Select a session to show it in the Perch view';
    pick.items = detached.map((s) => ({
      id: s.id,
      label: displayName(s),
      description: describe(s, ctx),
      buttons: [killButton],
    }));
    pick.onDidTriggerItemButton((e) => {
      pick.hide();
      void guard(kill)(e.item.id);
    });
    pick.onDidAccept(() => {
      const chosen = pick.selectedItems[0];
      pick.hide();
      if (chosen === undefined) return;
      const item = provider.item(chosen.id);
      if (item !== undefined) void view.reveal(item, { select: true, focus: true });
    });
    pick.onDidHide(() => pick.dispose());
    pick.show();
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('perch.refresh', () => scheduler.now()),
    vscode.commands.registerCommand('perch.kill', guard(kill)),
    vscode.commands.registerCommand('perch.dismiss', guard(dismiss)),
    vscode.commands.registerCommand('perch.showLastOutput', guard(showLastOutput)),
    vscode.commands.registerCommand('perch.showDetached', guard(showDetached)),
    vscode.commands.registerCommand('perch.showCommandLinePath', () =>
      supported ? commandLine.show() : vscode.window.showErrorMessage(unsupportedMessage),
    ),
  );

  if (context.extensionMode !== vscode.ExtensionMode.Test) return {};
  return {
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
    },
  };
}

export function deactivate(): void {}

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
