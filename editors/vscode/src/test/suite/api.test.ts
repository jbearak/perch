// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import type { PerchApiError } from '../../core/api';
import type { Testing } from '../../vscode/activation';
import type { PerchApiV1, SessionInfo } from '../../vscode/api';
import { activated, bundledBinaryOrSkip, extensionId, info, perch, waitFor } from './helpers';

const caller = 'test.caller';

/** Resolves with the first event that passes `match`. */
function next<T>(event: vscode.Event<T>, match: (e: T) => boolean, timeoutMs = 15_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error('timed out waiting for an event'));
    }, timeoutMs);
    const sub = event((e) => {
      if (!match(e)) return;
      clearTimeout(timer);
      sub.dispose();
      resolve(e);
    });
  });
}

async function rejectsWith(p: Promise<unknown>, code: string): Promise<PerchApiError> {
  try {
    await p;
  } catch (err) {
    assert.strictEqual((err as PerchApiError).code, code, String(err));
    return err as PerchApiError;
  }
  assert.fail(`expected a rejection with ${code}`);
}

describe('API', () => {
  let testing: Testing;
  let binary: string;
  let api: PerchApiV1;
  let tmp: string;
  const ids: string[] = [];

  before(async function () {
    const a = await activated();
    testing = a.testing;
    binary = bundledBinaryOrSkip(this, a.ext.extensionPath);
    await testing.terminalsStarted;
    const got = a.ext.exports.getApi(1, caller);
    assert.ok(got);
    api = got;
    tmp = fs.mkdtempSync('/tmp/perch-api-');
  });

  after(async () => {
    for (const id of ids) await perch(binary, ['kill', '--force', id]).catch(() => undefined);
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  it('getApi(1) returns version 1; getApi(2) returns undefined', async () => {
    const ext = vscode.extensions.getExtension(extensionId);
    assert.ok(ext);
    const exports = ext.exports as { getApi(version: number, callerId: string): PerchApiV1 | undefined };
    const v1 = exports.getApi(1, caller);
    assert.ok(v1);
    assert.strictEqual(v1.version, 1);
    assert.strictEqual(exports.getApi(2, caller), undefined);
    assert.strictEqual(exports.getApi(0, caller), undefined);
    assert.strictEqual(await v1.available(), true);
  });

  describe('a session created through the API', () => {
    let id: string;
    let terminal: vscode.Terminal;
    const received = () => path.join(tmp, 'received.txt');

    it('creates it, owned by the caller, and reports the attach', async () => {
      const attached = next(api.onDidAttach, (e) => e.session.owner === caller && e.session.name === 'api cat');
      const r = await api.createSession({
        name: 'api cat',
        kind: 'test',
        // Writes what it reads, so a send can be checked.
        command: ['sh', '-c', 'exec cat > "$0"', received()],
        annotations: { 'test.key': 'v' },
        show: false,
      });
      id = r.session.id;
      ids.push(id);
      terminal = r.terminal;
      assert.strictEqual(r.session.state, 'running');
      assert.strictEqual(r.session.owner, caller);
      assert.strictEqual(r.session.kind, 'test');
      assert.deepStrictEqual(r.session.annotations, { 'test.key': 'v' });
      assert.strictEqual(path.basename(r.session.dir), id);
      const e = await attached;
      assert.strictEqual(e.session.id, id);
      assert.strictEqual(e.terminal, terminal);
      assert.strictEqual(e.session.attachedHere, true);
    });

    it('maps the terminal to it, and lists it by owner', async () => {
      assert.strictEqual(api.sessionForTerminal(terminal)?.id, id);
      assert.ok(api.terminals().some((t) => t.terminal === terminal && t.session.id === id));
      const mine = await api.listSessions({ owner: caller });
      assert.ok(mine.some((s) => s.id === id));
      assert.ok(mine.every((s) => s.owner === caller));
      assert.deepStrictEqual(await api.listSessions({ owner: caller, kind: 'no such kind' }), []);
      const other = vscode.window.createTerminal({ name: 'not perch' });
      assert.strictEqual(api.sessionForTerminal(other), undefined);
      other.dispose();
    });

    it('an annotation-only change fires onDidChangeSessions; an unchanged listing does not', async () => {
      await testing.refresh();
      await testing.refresh();
      let fired = 0;
      const sub = api.onDidChangeSessions(() => fired++);
      try {
        await testing.refresh();
        assert.strictEqual(fired, 0, 'an unchanged listing');
        await perch(binary, ['annotate', id, 'test.only=1']);
        await testing.refresh();
        assert.strictEqual(fired, 1, 'an annotation-only change');
      } finally {
        sub.dispose();
        await perch(binary, ['annotate', id, '--unset', 'test.only']);
      }
    });

    it('annotates', async () => {
      await api.annotate(id, { 'test.added': 'a=b' }, ['test.key']);
      const s = (await api.listSessions({ owner: caller })).find((x) => x.id === id);
      assert.deepStrictEqual(s?.annotations, { 'test.added': 'a=b' });
    });

    it('sends while the terminal is closed', async () => {
      terminal.dispose();
      await waitFor('the session to detach', async () => {
        const i = await info(binary, id);
        return i?.state === 'running' && i.attached === false ? true : undefined;
      });
      assert.strictEqual(await api.send(id, 'hello from the API', { submit: true }), 'queued');
      const text = await waitFor('the program to receive the send', async () => {
        const t = fs.existsSync(received()) ? fs.readFileSync(received(), 'utf8') : '';
        return t.includes('\n') ? t : undefined;
      });
      assert.strictEqual(text, 'hello from the API\n');
    });

    it('kills it, and onDidEndSession fires once', async () => {
      const ends: SessionInfo[] = [];
      const sub = api.onDidEndSession((s) => {
        if (s.id === id) ends.push(s);
      });
      try {
        await api.kill(id);
        await waitFor('onDidEndSession', async () => (ends.length > 0 ? true : undefined));
        assert.ok(ends[0].state === 'exited' || ends[0].state === 'lost', ends[0].state);
        await testing.refresh();
        await testing.refresh();
        assert.strictEqual(ends.length, 1);
      } finally {
        sub.dispose();
      }
      await rejectsWith(api.kill(id), 'no_session');
    });
  });

  it('onDidEndSession reports, once, a session that starts and exits between listings', async () => {
    // A profile-started session: nothing lists it before its program exits.
    const { id, options } = api.prepareLaunch({ name: 'api quick exit', kind: 'test', command: ['sh', '-c', 'sleep 1; exit 5'] });
    ids.push(id);
    const ends: SessionInfo[] = [];
    const sub = api.onDidEndSession((s) => {
      if (s.id === id) ends.push(s);
    });
    try {
      const attached = next(api.onDidAttach, (e) => e.session.id === id);
      const terminal = vscode.window.createTerminal(options);
      await attached;
      assert.ok(!testing.sessions().some((s) => s.id === id), 'not listed yet');
      await waitFor('onDidEndSession', async () => (ends.length > 0 ? true : undefined));
      assert.strictEqual(ends[0].state, 'exited');
      assert.deepStrictEqual(ends[0].exit, { code: 5 });
      await testing.refresh();
      await testing.refresh();
      assert.strictEqual(ends.length, 1);
      terminal.dispose();
    } finally {
      sub.dispose();
    }
  });

  it('kill while attached here: onDidEndSession fires exactly once', async () => {
    const r = await api.createSession({ name: 'api kill attached', kind: 'test', command: ['sleep', '600'], show: false });
    ids.push(r.session.id);
    const ends: SessionInfo[] = [];
    const sub = api.onDidEndSession((s) => {
      if (s.id === r.session.id) ends.push(s);
    });
    try {
      await api.kill(r.session.id);
      await waitFor('onDidEndSession', async () => (ends.length > 0 ? true : undefined));
      await waitFor('the terminal to close', async () => (r.terminal.exitStatus !== undefined ? true : undefined));
      await testing.refresh();
      await testing.refresh();
      assert.strictEqual(ends.length, 1);
    } finally {
      sub.dispose();
    }
  });

  it('onDidEndSession reports a detached session whose program exits', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'api exit', '--', 'sh', '-c', 'sleep 2; exit 3'])).trim();
    ids.push(id);
    await testing.refresh();
    const ended = next(api.onDidEndSession, (s) => s.id === id);
    const timer = setInterval(() => void testing.refresh(), 500);
    try {
      const s = await ended;
      assert.strictEqual(s.state, 'exited');
      assert.deepStrictEqual(s.exit, { code: 3 });
    } finally {
      clearInterval(timer);
    }
  });

  describe('send error codes', () => {
    it('no_session for a session that does not exist or has ended', async () => {
      await rejectsWith(api.send('zzzzzzzz', 'x'), 'no_session');
    });

    it('queue_full once the session’s queue has no room, and nothing is queued', async () => {
      // A program that reads nothing, so the queue fills.
      const id = (await perch(binary, ['new', '-d', '--name', 'api full', '--', 'sh', '-c', 'stty raw -echo; exec sleep 600'])).trim();
      ids.push(id);
      await waitFor('the program to be sleep', async () => {
        const i = await info(binary, id);
        return (i?.foreground as { name?: string } | undefined)?.name === 'sleep' ? true : undefined;
      });
      const chunk = 'x'.repeat(500_000);
      let full: PerchApiError | undefined;
      for (let i = 0; i < 10 && full === undefined; i++) {
        try {
          assert.strictEqual(await api.send(id, chunk), 'queued');
        } catch (err) {
          full = err as PerchApiError;
        }
      }
      assert.ok(full, 'a send was refused');
      assert.strictEqual(full.code, 'queue_full', String(full));
      await perch(binary, ['kill', '--force', id]);
      await waitFor('the session to end', async () => {
        const i = await info(binary, id);
        return i === undefined || i.state !== 'running' ? true : undefined;
      });
      await rejectsWith(api.send(id, 'x'), 'no_session');
    });

    it('unavailable when the binary cannot run', async () => {
      const config = vscode.workspace.getConfiguration('perch');
      await config.update('path', path.join(tmp, 'no-such-perch'), vscode.ConfigurationTarget.Global);
      try {
        await waitFor('the setting to apply', async () => ((await api.available()) ? undefined : true));
        await rejectsWith(api.send('zzzzzzzz', 'x'), 'unavailable');
      } finally {
        await config.update('path', undefined, vscode.ConfigurationTarget.Global);
      }
      assert.strictEqual(await api.available(), true);
    });
  });

  describe('prepareLaunch', () => {
    it('serves another extension’s TerminalProfileProvider; sessionForTerminal maps its terminal', async () => {
      let prepared: { id: string; options: vscode.TerminalOptions } | undefined;
      // What another extension registers for its own profile.
      const provider: vscode.TerminalProfileProvider = {
        provideTerminalProfile: () => {
          prepared = api.prepareLaunch({ name: 'api profile', kind: 'test', command: ['sleep', '600'] });
          return new vscode.TerminalProfile(prepared.options);
        },
      };
      const profile = await provider.provideTerminalProfile(new vscode.CancellationTokenSource().token);
      assert.ok(profile && prepared);
      const id = prepared.id;
      ids.push(id);
      assert.strictEqual(prepared.options.isTransient, true);
      const attached = next(api.onDidAttach, (e) => e.session.id === id);
      // VS Code creates a profile's terminal from its options.
      const terminal = vscode.window.createTerminal(profile.options as vscode.TerminalOptions);
      const early = await waitFor('Perch to recognize the terminal', async () => api.sessionForTerminal(terminal));
      assert.strictEqual(early.id, id);
      assert.strictEqual(early.owner, caller);
      assert.strictEqual(path.basename(early.dir), id);
      const s = await api.waitForSession(id, terminal);
      assert.strictEqual(s.state, 'running');
      assert.strictEqual(s.owner, caller);
      assert.strictEqual((await attached).terminal, terminal);
      assert.strictEqual(api.sessionForTerminal(terminal)?.name, 'api profile');
      terminal.dispose();
    });

    it('Perch’s own profile, started by VS Code, is recognized and mapped', async () => {
      const opened = next(vscode.window.onDidOpenTerminal, (t) => t.name === 'Persistent shell');
      // What picking "Persistent shell" in the profile menu runs.
      await vscode.commands.executeCommand('workbench.action.terminal.newWithProfile', {
        config: { extensionIdentifier: extensionId, id: 'perch.persistentShell', title: 'Persistent shell' },
      });
      const terminal = await opened;
      const early = await waitFor('Perch to recognize the terminal', async () => api.sessionForTerminal(terminal));
      ids.push(early.id);
      const s = await api.waitForSession(early.id, terminal);
      assert.strictEqual(s.state, 'running');
      assert.strictEqual(s.kind, 'shell');
      assert.strictEqual(s.owner, undefined);
      terminal.dispose();
    });

    it('rejects bad options and does not let a caller set the owner', () => {
      assert.throws(() => api.prepareLaunch({ name: 'x', kind: 'x', command: [] }), TypeError);
      const { options } = api.prepareLaunch({
        name: 'x',
        kind: 'x',
        command: ['true'],
        owner: 'someone.else',
      } as Parameters<PerchApiV1['prepareLaunch']>[0]);
      const args = options.shellArgs as string[];
      assert.strictEqual(args[args.indexOf('--owner') + 1], caller);
    });
  });

  it('attach opens a terminal on a detached session, and shows the existing one after', async () => {
    const id = (await perch(binary, ['new', '-d', '--name', 'api attach', '--', 'sleep', '600'])).trim();
    ids.push(id);
    const attached = next(api.onDidAttach, (e) => e.session.id === id);
    const t = await api.attach(id, { show: false });
    const e = await attached;
    assert.strictEqual(e.terminal, t);
    assert.strictEqual(e.session.attachedHere, true);
    assert.strictEqual(await api.attach(id), t);
    assert.strictEqual(api.sessionForTerminal(t)?.id, id);
    await rejectsWith(api.attach('zzzzzzzz'), 'no_session');
    t.dispose();
  });
});
