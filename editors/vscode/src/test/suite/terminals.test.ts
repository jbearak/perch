// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { recognize } from '../../core/launch';
import type { Testing } from '../../vscode/activation';
import type { TerminalChoice } from '../../vscode/terminals';
import { activated, bundledBinaryOrSkip, info, nextClose, perch, waitFor } from './helpers';

/** A client in "another window": a terminal Perch does not recognize as its own. */
function otherClient(binary: string, id: string): vscode.Terminal {
  return vscode.window.createTerminal({
    name: 'other window',
    shellPath: '/bin/sh',
    shellArgs: ['-c', `exec "${binary}" attach ${id} --label fedcba9876543210#bbbbbbbb --detach-key none`],
  });
}

async function attachedBy(binary: string, id: string, label: string): Promise<void> {
  await waitFor(`${id} attached by ${label}`, async () => {
    const i = await info(binary, id);
    return i?.attached === true && i.clientLabel === label ? true : undefined;
  });
}

async function detached(binary: string, id: string): Promise<void> {
  await waitFor(`${id} detached`, async () => {
    const i = await info(binary, id);
    return i?.state === 'running' && i.attached === false ? true : undefined;
  });
}

describe('terminals', () => {
  let testing: Testing;
  let binary: string;
  const ids: string[] = [];
  const others: vscode.Terminal[] = [];

  before(async function () {
    const a = await activated();
    testing = a.testing;
    binary = bundledBinaryOrSkip(this, a.ext.extensionPath);
    await testing.terminalsStarted;
    await vscode.workspace.getConfiguration('terminal.integrated').update('scrollback', 4321, vscode.ConfigurationTarget.Global);
  });

  after(async () => {
    testing?.setConfirm(undefined);
    testing?.setNotify(undefined);
    for (const t of others) t.dispose();
    for (const id of ids) await perch(binary, ['kill', '--force', id]).catch(() => undefined);
    await vscode.workspace.getConfiguration('terminal.integrated').update('scrollback', undefined, vscode.ConfigurationTarget.Global);
  });

  describe('create, close and reopen', () => {
    let id: string;
    let terminal: vscode.Terminal;

    it('creates a session in a transient terminal and waits until it runs', async () => {
      const r = await testing.createSession({ name: 'e2e create', kind: 'shell', command: ['sleep', '600'] });
      id = r.session.id;
      ids.push(id);
      terminal = r.terminal;
      assert.strictEqual(r.session.state, 'running');
      const o = terminal.creationOptions as vscode.TerminalOptions;
      assert.strictEqual(o.isTransient, true);
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'new', ifDetached: false, label: testing.label });
      const args = o.shellArgs as string[];
      for (const [flag, value] of [['--scrollback', '4321'], ['--detach-key', 'none'], ['--restore', 'full'], ['--label', testing.label]]) {
        assert.strictEqual(args[args.indexOf(flag) + 1], value, `${flag} ${value}`);
      }
      await attachedBy(binary, id, testing.label);
      assert.strictEqual(testing.terminalFor(id), terminal);
      assert.ok(testing.restoreSet().includes(id));
      const i = await info(binary, id);
      assert.strictEqual(i?.name, 'e2e create');
      assert.strictEqual(i?.kind, 'shell');
    });

    it('closing the terminal detaches; the session survives and the first close says so', async () => {
      const notices: { message: string; items: string[] }[] = [];
      testing.setNotify(async (message, ...items) => {
        notices.push({ message, items });
        return undefined;
      });
      terminal.show();
      await waitFor('the terminal to be active', async () => (vscode.window.activeTerminal === terminal ? true : undefined));
      const closed = nextClose(testing, id);
      await vscode.commands.executeCommand('workbench.action.terminal.kill');
      const outcome = await closed;
      assert.strictEqual(outcome.reason, 'user');
      assert.strictEqual(outcome.result, 'forget');
      await detached(binary, id);
      assert.ok(!testing.restoreSet().includes(id), 'a terminal the user closed is not restored');
      await waitFor('the first-close notice', async () => (notices.length > 0 ? true : undefined));
      assert.match(notices[0].message, /e2e create is still running/);
      assert.deepStrictEqual(notices[0].items, ['Kill', "Don't Show Again"]);
    });

    it('restores a detached session in the restore set', async () => {
      await testing.addToRestoreSet(id);
      await testing.restore();
      const t = await waitFor('a restored terminal', async () => testing.terminalFor(id));
      const o = t.creationOptions as vscode.TerminalOptions;
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: true, label: testing.label });
      assert.strictEqual(o.isTransient, true);
      assert.strictEqual(t.name, 'e2e create');
      await attachedBy(binary, id, testing.label);
    });

    it('a busy restore exits with 75, closes quietly and keeps the session', async () => {
      const t = testing.terminalFor(id);
      assert.ok(t);
      // Another window takes it over; this window's terminal keeps running.
      const other = otherClient(binary, id);
      others.push(other);
      await attachedBy(binary, id, 'fedcba9876543210#bbbbbbbb');
      const disposed = nextClose(testing, id);
      t.dispose();
      assert.strictEqual((await disposed).result, 'ignore');
      const closed = nextClose(testing, id);
      testing.restoreAttach(id);
      const outcome = await closed;
      assert.strictEqual(outcome.reason, 'process');
      assert.strictEqual(outcome.code, 75);
      assert.strictEqual(outcome.result, 'busy');
      assert.ok(testing.restoreSet().includes(id), 'still in the restore set');
      assert.strictEqual(testing.terminalFor(id), undefined);
      // Restore leaves a session attached from another workspace where it is.
      await testing.restore();
      assert.strictEqual(testing.terminalFor(id), undefined);
      assert.deepStrictEqual(testing.pendingRestores(), []);
      await attachedBy(binary, id, 'fedcba9876543210#bbbbbbbb');
    });

    it('open asks before moving a session from another window', async () => {
      const asked: string[] = [];
      testing.setConfirm(async (message) => {
        asked.push(message);
        return false;
      });
      await vscode.commands.executeCommand('perch.open', id);
      assert.deepStrictEqual(asked, ['Shell: e2e create is open in another window. Move it here?']);
      assert.strictEqual(testing.terminalFor(id), undefined);
      testing.setConfirm(async () => true);
      await vscode.commands.executeCommand('perch.open', id);
      const t = testing.terminalFor(id);
      assert.ok(t);
      const o = t.creationOptions as vscode.TerminalOptions;
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: false, label: testing.label });
      await attachedBy(binary, id, testing.label);
    });
  });

  it('opens a detached session without asking', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'e2e open', '--kind', 'python', '--', 'sleep', '600'])).trim();
    ids.push(id);
    await testing.refresh();
    let asked = false;
    testing.setConfirm(async () => (asked = true));
    await vscode.commands.executeCommand('perch.open', id);
    assert.strictEqual(asked, false);
    assert.ok(testing.terminalFor(id));
    await attachedBy(binary, id, testing.label);
    // Opening it again shows the same terminal.
    const t = testing.terminalFor(id);
    await vscode.commands.executeCommand('perch.open', id);
    assert.strictEqual(testing.terminalFor(id), t);
  });

  it('open asks if another window attached after the check, and declining leaves it there', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'e2e race', '--', 'sleep', '600'])).trim();
    ids.push(id);
    await testing.refresh(); // The listing says detached.
    const other = otherClient(binary, id);
    others.push(other);
    await attachedBy(binary, id, 'fedcba9876543210#bbbbbbbb');
    const asked: string[] = [];
    let answered!: () => void;
    const answeredOnce = new Promise<void>((r) => (answered = r));
    testing.setConfirm(async (message) => {
      asked.push(message);
      answered();
      return false;
    });
    const closed = nextClose(testing, id);
    const t = await testing.openStale(id);
    assert.ok(t, 'a guarded attach starts');
    const o = t.creationOptions as vscode.TerminalOptions;
    assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: true, label: testing.label });
    const outcome = await closed;
    assert.strictEqual(outcome.code, 75);
    assert.strictEqual(outcome.result, 'busy');
    await answeredOnce;
    assert.deepStrictEqual(asked, ['Program: e2e race is open in another window. Move it here?']);
    assert.strictEqual(testing.terminalFor(id), undefined);
    await attachedBy(binary, id, 'fedcba9876543210#bbbbbbbb');
  });

  it('a program that really exits with 75 ends its session and leaves the restore set', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'e2e exit 75', '--', 'sh', '-c', 'sleep 4; exit 75'])).trim();
    ids.push(id);
    await testing.refresh();
    const closed = nextClose(testing, id);
    testing.restoreAttach(id);
    await attachedBy(binary, id, testing.label);
    const outcome = await closed;
    assert.strictEqual(outcome.reason, 'process');
    assert.strictEqual(outcome.code, 75);
    assert.strictEqual(outcome.result, 'ended');
    assert.ok(!testing.restoreSet().includes(id));
  });

  it('recognizes a terminal from prepareLaunch in onDidOpenTerminal', async () => {
    const { id, options } = await testing.prepareLaunch({ name: 'e2e profile', kind: 'shell', command: ['sleep', '600'] });
    ids.push(id);
    const t = vscode.window.createTerminal(options);
    others.push(t);
    await waitFor('the terminal to be tracked', async () => testing.terminalFor(id));
    assert.strictEqual(testing.terminalFor(id), t);
    assert.ok(testing.restoreSet().includes(id));
    await attachedBy(binary, id, testing.label);
  });

  it('reports a command that cannot run', async () => {
    await assert.rejects(
      testing.createSession({ name: 'e2e missing', kind: '', command: ['/nonexistent/perch-test-program'] }),
      /e2e missing could not start/,
    );
  });

  it('starts the persistent shell from perch.shell', async () => {
    await vscode.workspace.getConfiguration('perch').update('shell', ['/bin/sh', '-i'], vscode.ConfigurationTarget.Global);
    try {
      assert.deepStrictEqual(testing.shellCommand(), ['/bin/sh', '-i']);
    } finally {
      await vscode.workspace.getConfiguration('perch').update('shell', undefined, vscode.ConfigurationTarget.Global);
    }
    const shell = testing.shellCommand();
    assert.strictEqual(shell[0], process.env.SHELL || '/bin/sh');
  });

  describe('new persistent terminal', () => {
    const os_ = process.platform === 'darwin' ? 'osx' : 'linux';
    const config = () => vscode.workspace.getConfiguration('terminal.integrated');
    const global = vscode.ConfigurationTarget.Global;
    let dir: string;
    const pickLabel = (label: string) => (choices: TerminalChoice[]) => Promise.resolve(choices.find((c) => c.label === label));
    const readFile = (file: string) => waitFor(file, async () => (fs.existsSync(file) ? fs.readFileSync(file, 'utf8') || undefined : undefined));
    // Writes what the program saw to its first argument's file, then waits.
    const report = 'printf "%s|%s|%s|%s\\n" "$0" "$PERCH_T_SET" "$PERCH_T_HOME" "${PERCH_T_DROP-unset}" > "$1"; exec sleep 600';

    before(async () => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'perch-profiles-'));
      // The variable a profile unsets has to be in the terminal's environment first.
      await config().update(`env.${os_}`, { PERCH_T_DROP: 'present' }, global);
      await config().update(
        `profiles.${os_}`,
        {
          'e2e profile': {
            path: ['/nonexistent/sh', '/bin/sh'],
            args: ['-c', report, 'zero{x}{dir}', path.join(dir, 'profile.txt')],
            env: { PERCH_T_SET: 'set {id}', PERCH_T_HOME: '${env:HOME}', PERCH_T_DROP: null },
          },
          'e2e control': { path: 'sh', args: ['-c', report, 'control', path.join(dir, 'control.txt')] },
          'e2e missing': { path: '/nonexistent/sh' },
          'e2e command variable': { path: '/bin/sh', args: ['${command:pick}'] },
        },
        global,
      );
    });

    after(async () => {
      for (const key of [`env.${os_}`, `profiles.${os_}`, `defaultProfile.${os_}`]) await config().update(key, undefined, global);
      fs.rmSync(dir, { recursive: true, force: true });
    });

    it('lists the persistent shell, the profiles Perch can run, and a command line', async () => {
      let labels: string[] = [];
      await testing.newTerminal((choices) => {
        labels = choices.map((c) => c.label);
        return Promise.resolve(undefined);
      });
      assert.strictEqual(labels[0], 'Persistent shell');
      assert.strictEqual(labels.at(-1), 'Run a command...');
      assert.ok(labels.includes('e2e profile') && labels.includes('e2e control'), labels.join(', '));
      assert.ok(!labels.includes('e2e missing'), 'a profile whose path is missing');
      assert.ok(!labels.includes('e2e command variable'), 'a profile with a variable Perch cannot resolve');
    });

    it('runs a profile with its arguments, variables and environment, braces kept', async () => {
      const r = await testing.newTerminal(pickLabel('e2e profile'));
      assert.ok(r);
      ids.push(r.session.id);
      assert.strictEqual(r.session.name, 'e2e profile');
      assert.strictEqual(r.session.kind, 'shell');
      const seen = await readFile(path.join(dir, 'profile.txt'));
      assert.strictEqual(seen, `zero{x}{dir}|set {id}|${process.env.HOME}|unset\n`);
      const control = await testing.newTerminal(pickLabel('e2e control'));
      assert.ok(control);
      ids.push(control.session.id);
      assert.notStrictEqual(control.session.id, r.session.id);
      assert.strictEqual(await readFile(path.join(dir, 'control.txt')), 'control|||present\n');
    });

    it('runs a command line typed by the user, braces kept', async () => {
      const file = path.join(dir, 'command.txt');
      const line = `printf '%s\\n' '{x} {dir}' > '${file}'; exec sleep 600`;
      const r = await testing.newTerminal(pickLabel('Run a command...'), () => Promise.resolve(line));
      assert.ok(r);
      ids.push(r.session.id);
      assert.strictEqual(r.session.kind, '');
      assert.strictEqual(await readFile(file), '{x} {dir}\n');
      // Cancelling the prompt creates nothing.
      assert.strictEqual(await testing.newTerminal(pickLabel('Run a command...'), () => Promise.resolve(undefined)), undefined);
    });

    it('makes every new terminal persistent when Persistent shell is the default profile', async () => {
      await config().update(`defaultProfile.${os_}`, 'Persistent shell', global);
      const before = new Set(vscode.window.terminals);
      await vscode.commands.executeCommand('workbench.action.terminal.new');
      await vscode.commands.executeCommand('workbench.action.terminal.new');
      const created = await waitFor('two new terminals', async () => {
        const t = vscode.window.terminals.filter((x) => !before.has(x));
        return t.length >= 2 ? t : undefined;
      });
      const launched = created.map((t) => {
        others.push(t);
        const o = t.creationOptions as vscode.TerminalOptions;
        return recognize(o.shellPath, o.shellArgs);
      });
      for (const l of launched) {
        assert.strictEqual(l?.mode, 'new', 'a Perch terminal creating its session');
        ids.push(l.id);
      }
      assert.notStrictEqual(launched[0]?.id, launched[1]?.id);
      for (const l of launched) await attachedBy(binary, l!.id, testing.label);
    });
  });
});
