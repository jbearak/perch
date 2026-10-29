// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as vscode from 'vscode';
import type { Testing } from '../../vscode/activation';
import { activated, bundledBinaryOrSkip, perch, waitFor } from './helpers';

describe('session list', () => {
  let testing: Testing;
  let binary: string;
  let id: string;

  before(async function () {
    const a = await activated();
    testing = a.testing;
    binary = bundledBinaryOrSkip(this, a.ext.extensionPath);
    id = (await perch(binary, ['new', '-d', '--name', 'e2e sleeper', '--kind', 'shell', '--', 'sleep', '600'])).trim();
  });

  after(async () => {
    testing?.setConfirm(undefined);
    if (id !== undefined) await perch(binary, ['kill', '--force', id]).catch(() => undefined);
  });

  it('lists a session the CLI created, detached', async () => {
    await testing.refresh();
    const s = testing.sessions().find((x) => x.id === id);
    assert.ok(s, `${id} is listed`);
    assert.strictEqual(s.name, 'e2e sleeper');
    assert.strictEqual(s.kind, 'shell');
    assert.strictEqual(s.state, 'running');
    assert.strictEqual(s.attached, false);
    assert.ok(testing.detachedCount() >= 1);
  });

  it('asks before killing, and keeps the session when declined', async () => {
    const asked: { message: string; detail: string; action: string }[] = [];
    testing.setConfirm(async (message, detail, action) => {
      asked.push({ message, detail, action });
      return false;
    });
    await vscode.commands.executeCommand('perch.kill', id);
    assert.strictEqual(asked.length, 1);
    assert.strictEqual(asked[0].message, 'Kill e2e sleeper?');
    assert.strictEqual(asked[0].action, 'Kill');
    for (const part of ['Kind: Shell', 'Working directory:', 'Age:', 'Memory:']) {
      assert.ok(asked[0].detail.includes(part), `the confirmation shows ${part}`);
    }
    const info = JSON.parse(await perch(binary, ['info', id, '--json']));
    assert.strictEqual(info.state, 'running');
  });

  it('kills once confirmed, then dismisses the tombstone', async () => {
    testing.setConfirm(async () => true);
    await vscode.commands.executeCommand('perch.kill', id);
    const ended = await waitFor('the session to end', async () => {
      await testing.refresh();
      const s = testing.sessions().find((x) => x.id === id);
      return s === undefined || s.state === 'exited' || s.state === 'lost' ? { s } : undefined;
    });
    if (ended.s !== undefined) {
      await vscode.commands.executeCommand('perch.dismiss', id);
      assert.strictEqual(
        testing.sessions().find((x) => x.id === id),
        undefined,
        'dismissed sessions leave the list',
      );
    }
  });
});
