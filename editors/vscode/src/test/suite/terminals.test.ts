// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as vscode from 'vscode';
import { recognize } from '../../core/launch';
import type { Testing } from '../../vscode/activation';
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
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'new', ifDetached: false });
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
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: true });
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
      assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: false });
      await attachedBy(binary, id, testing.label);
    });
  });

  it('opens a detached session without asking', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'e2e open', '--kind', 'r', '--', 'sleep', '600'])).trim();
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
    assert.deepStrictEqual(recognize(o.shellPath, o.shellArgs), { id, mode: 'attach', ifDetached: true });
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
});
