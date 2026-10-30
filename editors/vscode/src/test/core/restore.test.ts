// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { attachArgs, newArgs, recognize } from '../../core/launch';
import {
  adopt,
  after75,
  closeAction,
  openStep,
  RESTORE_WINDOW_MS,
  RestoreSet,
  RestoreStore,
  Restorer,
  restoreStep,
  settle75,
  TerminalRegistry,
} from '../../core/restore';
import { Session } from '../../core/sessions';

const ws = '0123456789abcdef';
const label = `${ws}#aaaaaaaa`;

function session(id: string, over: Partial<Session> = {}): Session {
  return {
    id, name: id, kind: 'shell', annotations: {}, dir: `/tmp/${id}`, degraded: [], cwd: '/',
    host: 'h', createdAt: '2026-10-29T00:00:00Z', state: 'running', attached: false, ...over,
  };
}

/** workspaceState, in memory. */
function memoryStore(ids: string[] = []): RestoreStore & { ids: readonly string[] } {
  const s = {
    ids: ids as readonly string[],
    get: () => s.ids,
    set: async (next: readonly string[]) => {
      s.ids = [...next];
    },
  };
  return s;
}

describe('restore set', () => {
  it('adds once, in order, and removes', async () => {
    const store = memoryStore();
    const set = new RestoreSet(store);
    await set.add('aaaa1111');
    await set.add('bbbb2222');
    await set.add('aaaa1111');
    assert.deepStrictEqual(store.ids, ['aaaa1111', 'bbbb2222']);
    assert.ok(set.has('bbbb2222'));
    await set.remove('aaaa1111');
    await set.remove('cccc3333');
    assert.deepStrictEqual(set.ids(), ['bbbb2222']);
  });

  it('decides what a closed terminal means', () => {
    assert.strictEqual(closeAction('user', undefined), 'forget');
    assert.strictEqual(closeAction('process', 0), 'forget');
    assert.strictEqual(closeAction('process', 1), 'forget');
    assert.strictEqual(closeAction('process', undefined), 'forget');
    assert.strictEqual(closeAction('process', 75), 'confirm');
    assert.strictEqual(closeAction('shutdown', undefined), 'keep');
    assert.strictEqual(closeAction('unknown', 0), 'keep');
    assert.strictEqual(closeAction('extension', 75), 'ignore');
  });

  it('confirms an exit with 75 with perch info', () => {
    assert.strictEqual(after75(session('a', { attached: true, clientLabel: 'tty:/dev/ttys001' })), 'busy');
    // The winning client may have detached before the check: still live.
    assert.strictEqual(after75(session('a', { attached: false })), 'busy');
    assert.strictEqual(after75(session('a', { state: 'unresponsive' })), 'busy');
    // Nothing is known when info fails: keep it.
    assert.strictEqual(after75(new Error('timed out')), 'busy');
    // A program can exit with 75 too: then the session has ended or is gone.
    assert.strictEqual(after75(undefined), 'ended');
    assert.strictEqual(after75(session('a', { state: 'exited', exit: { code: 75 } })), 'ended');
    assert.strictEqual(after75(session('a', { state: 'lost' })), 'ended');
  });

  it('decides how Open attaches', () => {
    assert.strictEqual(openStep(session('a'), false), 'attach-guarded');
    assert.strictEqual(openStep(session('a', { attached: true }), false), 'confirm');
    assert.strictEqual(openStep(session('a', { attached: true }), true), 'attach');
    assert.strictEqual(openStep(session('a'), true), 'attach');
    assert.strictEqual(openStep(session('a', { state: 'exited' }), false), 'not-running');
    assert.strictEqual(openStep(session('a', { state: 'unresponsive' }), true), 'not-running');
  });

  it('decides each session restore step', () => {
    assert.strictEqual(restoreStep(undefined, label), 'forget');
    assert.strictEqual(restoreStep(session('a', { state: 'exited' }), label), 'forget');
    assert.strictEqual(restoreStep(session('a', { state: 'lost' }), label), 'forget');
    assert.strictEqual(restoreStep(session('a'), label), 'open');
    assert.strictEqual(restoreStep(session('a', { state: 'starting' }), label), 'retry');
    assert.strictEqual(restoreStep(session('a', { state: 'unresponsive' }), label), 'retry');
    assert.strictEqual(restoreStep(session('a', { attached: true, clientLabel: label }), label), 'done');
    // The previous incarnation of this window may still be exiting.
    assert.strictEqual(restoreStep(session('a', { attached: true, clientLabel: `${ws}#bbbbbbbb` }), label), 'retry');
    assert.strictEqual(restoreStep(session('a', { attached: true, clientLabel: 'fedcba9876543210#bbbbbbbb' }), label), 'leave');
    assert.strictEqual(restoreStep(session('a', { attached: true, clientLabel: 'tty:/dev/pts/3' }), label), 'leave');
    assert.strictEqual(restoreStep(session('a', { attached: true }), label), 'leave');
    // An empty window's key matches nothing of a workspace's.
    assert.strictEqual(restoreStep(session('a', { attached: true, clientLabel: 'none#bbbbbbbb' }), 'none#aaaaaaaa'), 'retry');
  });
});

/** A restorer over a fake clock, sessions and terminals. */
function restorer(ids: string[], sessions: Map<string, Session>, terminals = new Set<string>()) {
  let t = 1000;
  const store = memoryStore(ids);
  const opened: string[] = [];
  const infoCalls: string[] = [];
  const r = new Restorer({
    set: new RestoreSet(store),
    label,
    info: async (id) => {
      infoCalls.push(id);
      const s = sessions.get(id);
      if (s instanceof Error) throw s;
      return s;
    },
    hasTerminal: (id) => terminals.has(id),
    open: (s) => {
      opened.push(s.id);
      terminals.add(s.id);
    },
    now: () => t,
    log: () => undefined,
  });
  return {
    r,
    store,
    opened,
    infoCalls,
    advance: (ms: number) => {
      t += ms;
    },
  };
}

describe('restorer', () => {
  it('checks info first and opens detached sessions, skipping adopted ones', async () => {
    const sessions = new Map([
      ['aaaa1111', session('aaaa1111')],
      ['bbbb2222', session('bbbb2222')],
      ['cccc3333', session('cccc3333', { state: 'exited', exit: { code: 0 } })],
    ]);
    const x = restorer(['aaaa1111', 'bbbb2222', 'cccc3333', 'dddd4444'], sessions, new Set(['bbbb2222']));
    await x.r.start();
    assert.deepStrictEqual(x.infoCalls, ['aaaa1111', 'cccc3333', 'dddd4444'], 'adopted sessions are not checked');
    assert.deepStrictEqual(x.opened, ['aaaa1111']);
    // Ended and missing sessions leave the set; adopted ones stay.
    assert.deepStrictEqual(x.store.ids, ['aaaa1111', 'bbbb2222']);
    assert.deepStrictEqual(x.r.pendingIds(), []);
  });

  it('never takes a session from a live client in another workspace', async () => {
    const sessions = new Map([['aaaa1111', session('aaaa1111', { attached: true, clientLabel: 'fedcba9876543210#bbbbbbbb' })]]);
    const x = restorer(['aaaa1111'], sessions);
    await x.r.start();
    assert.deepStrictEqual(x.opened, []);
    assert.deepStrictEqual(x.r.pendingIds(), []);
    assert.deepStrictEqual(x.store.ids, ['aaaa1111'], 'it stays in the set for a later activation');
  });

  it('retries a session attached with this workspace key for 60 seconds, then drops it', async () => {
    const s = session('aaaa1111', { attached: true, clientLabel: `${ws}#bbbbbbbb` });
    const sessions = new Map([['aaaa1111', s]]);
    const x = restorer(['aaaa1111'], sessions);
    await x.r.start();
    assert.deepStrictEqual(x.r.pendingIds(), ['aaaa1111']);
    x.advance(5000);
    await x.r.pass();
    assert.deepStrictEqual(x.opened, []);
    // The old window finishes exiting: the next refresh opens it.
    sessions.set('aaaa1111', session('aaaa1111'));
    x.advance(5000);
    await x.r.pass();
    assert.deepStrictEqual(x.opened, ['aaaa1111']);
    assert.deepStrictEqual(x.r.pendingIds(), []);

    const y = restorer(['aaaa1111'], new Map([['aaaa1111', s]]));
    await y.r.start();
    y.advance(RESTORE_WINDOW_MS + 1);
    await y.r.pass();
    assert.deepStrictEqual(y.r.pendingIds(), [], 'dropped for this activation');
    assert.deepStrictEqual(y.store.ids, ['aaaa1111'], 'but kept in the set');
    assert.strictEqual(y.r.retrying(), false);
  });

  it('keeps a busy session pending only while the window is open', async () => {
    const x = restorer([], new Map());
    await x.r.start();
    x.r.busy('aaaa1111');
    assert.deepStrictEqual(x.r.pendingIds(), ['aaaa1111']);
    const y = restorer([], new Map());
    await y.r.start();
    y.advance(RESTORE_WINDOW_MS + 1);
    y.r.busy('aaaa1111');
    assert.deepStrictEqual(y.r.pendingIds(), []);
  });

  it('keeps going when info fails, and retries', async () => {
    const sessions = new Map<string, Session>([['bbbb2222', session('bbbb2222')]]);
    const x = restorer(['aaaa1111', 'bbbb2222'], sessions);
    (sessions as Map<string, unknown>).set('aaaa1111', new Error('timed out'));
    await x.r.start();
    assert.deepStrictEqual(x.opened, ['bbbb2222']);
    assert.deepStrictEqual(x.r.pendingIds(), ['aaaa1111']);
    sessions.set('aaaa1111', session('aaaa1111'));
    await x.r.pass();
    assert.deepStrictEqual(x.opened, ['bbbb2222', 'aaaa1111']);
  });

  it('runs one pass at a time', async () => {
    const sessions = new Map([['aaaa1111', session('aaaa1111')]]);
    const x = restorer(['aaaa1111'], sessions);
    await Promise.all([x.r.start(), x.r.pass(), x.r.pass()]);
    assert.deepStrictEqual(x.opened, ['aaaa1111']);
  });

  it('restores nothing before it starts', async () => {
    const x = restorer(['aaaa1111'], new Map([['aaaa1111', session('aaaa1111')]]));
    await x.r.pass();
    assert.deepStrictEqual(x.opened, []);
  });
});

/** A terminal as VS Code reports it after an extension host restart. */
interface FakeTerminal {
  name: string;
  shellPath?: string;
  shellArgs?: string[] | string;
  exited?: boolean;
}

const recognizeFake = (t: FakeTerminal) => (t.exited ? undefined : recognize(t.shellPath, t.shellArgs));

describe('adoption', () => {
  const created: FakeTerminal = {
    name: 'R',
    shellPath: '/old/extension/bin/linux-x64/perch',
    shellArgs: newArgs({ id: 'aaaa1111', label, scrollback: 1000, name: 'R', kind: 'r', command: ['R'] }),
  };
  const opened: FakeTerminal = { name: 'shell', shellPath: '/x/perch', shellArgs: attachArgs('bbbb2222', label, 1000) };
  const plain: FakeTerminal = { name: 'zsh', shellPath: '/bin/zsh', shellArgs: ['-l'] };
  const exited: FakeTerminal = { name: 'gone', shellPath: '/x/perch', shellArgs: attachArgs('cccc3333', label, 1000), exited: true };

  it('adopts the Perch terminals open after an extension host restart', () => {
    const registry = new TerminalRegistry<FakeTerminal>();
    const adopted = adopt(registry, [plain, created, opened, exited], recognizeFake);
    assert.deepStrictEqual(adopted.map((a) => a.launch.id), ['aaaa1111', 'bbbb2222']);
    assert.strictEqual(registry.terminalFor('aaaa1111'), created);
    assert.strictEqual(registry.get(opened)?.mode, 'attach');
    assert.strictEqual(registry.get(plain), undefined);
  });

  it('is idempotent and independent of event order', () => {
    const registry = new TerminalRegistry<FakeTerminal>();
    // onDidOpenTerminal saw one before activation's pass did.
    registry.track(opened, recognizeFake(opened)!);
    const adopted = adopt(registry, [created, opened], recognizeFake);
    assert.deepStrictEqual(adopted.map((a) => a.launch.id), ['aaaa1111']);
    assert.deepStrictEqual(adopt(registry, [created, opened], recognizeFake), []);
    assert.strictEqual(registry.all().length, 2);
  });

  it('adopted terminals are not restored again', async () => {
    const registry = new TerminalRegistry<FakeTerminal>();
    adopt(registry, [created, opened], recognizeFake);
    const sessions = new Map([
      ['aaaa1111', session('aaaa1111', { attached: true, clientLabel: `${ws}#99999999` })],
      ['bbbb2222', session('bbbb2222')],
    ]);
    const x = restorer([], sessions);
    const r = new Restorer({
      set: new RestoreSet(memoryStore(['aaaa1111', 'bbbb2222'])),
      label,
      info: async (id) => sessions.get(id),
      hasTerminal: (id) => registry.terminalFor(id) !== undefined,
      open: (s) => x.opened.push(s.id),
      now: () => 0,
      log: () => undefined,
    });
    await r.start();
    assert.deepStrictEqual(x.opened, []);
    assert.deepStrictEqual(r.pendingIds(), []);
  });

  it('forgets a terminal once it closes', () => {
    const registry = new TerminalRegistry<FakeTerminal>();
    adopt(registry, [created], recognizeFake);
    assert.strictEqual(registry.untrack(created)?.id, 'aaaa1111');
    assert.strictEqual(registry.terminalFor('aaaa1111'), undefined);
    assert.strictEqual(registry.untrack(created), undefined);
  });
});

describe('an exit with 75', () => {
  /** A restore that found its session busy, then whatever info says next. */
  async function busyRestore(next: Session | undefined | Error, advance = 0) {
    const sessions = new Map<string, Session>();
    const terminals = new Set(['aaaa1111']);
    const x = restorer(['aaaa1111'], sessions, terminals);
    await x.r.start(); // The restore terminal is open, so nothing is pending.
    terminals.delete('aaaa1111'); // It exited with 75.
    const set = new RestoreSet(x.store);
    x.advance(advance);
    const result = await settle75(
      'aaaa1111',
      async () => {
        if (next instanceof Error) throw next;
        return next;
      },
      set,
      (id) => x.r.busy(id),
    );
    return { x, result, sessions };
  }

  it('retries a session whose winning client detached before the check', async () => {
    const { x, result, sessions } = await busyRestore(session('aaaa1111'));
    assert.strictEqual(result, 'busy');
    assert.deepStrictEqual(x.store.ids, ['aaaa1111']);
    assert.deepStrictEqual(x.r.pendingIds(), ['aaaa1111']);
    sessions.set('aaaa1111', session('aaaa1111'));
    await x.r.pass();
    assert.deepStrictEqual(x.opened, ['aaaa1111'], 'reopened on the next refresh');
  });

  it('keeps a session attached elsewhere without reopening it', async () => {
    const other = session('aaaa1111', { attached: true, clientLabel: 'fedcba9876543210#bbbbbbbb' });
    const { x, result, sessions } = await busyRestore(other);
    assert.strictEqual(result, 'busy');
    sessions.set('aaaa1111', other);
    await x.r.pass();
    assert.deepStrictEqual(x.opened, []);
    assert.deepStrictEqual(x.store.ids, ['aaaa1111']);
  });

  it('keeps a session when info fails, and retries it within the window', async () => {
    const { x, result, sessions } = await busyRestore(new Error('timed out'));
    assert.strictEqual(result, 'busy');
    assert.deepStrictEqual(x.store.ids, ['aaaa1111']);
    assert.deepStrictEqual(x.r.pendingIds(), ['aaaa1111']);
    sessions.set('aaaa1111', session('aaaa1111'));
    await x.r.pass();
    assert.deepStrictEqual(x.opened, ['aaaa1111']);

    const late = await busyRestore(new Error('timed out'), RESTORE_WINDOW_MS + 1);
    assert.strictEqual(late.result, 'busy');
    assert.deepStrictEqual(late.x.store.ids, ['aaaa1111'], 'kept for a later activation');
    assert.deepStrictEqual(late.x.r.pendingIds(), [], 'not retried after the window');
  });

  it('forgets a session that ended or is gone', async () => {
    for (const next of [session('aaaa1111', { state: 'exited', exit: { code: 75 } }), session('aaaa1111', { state: 'lost' }), undefined]) {
      const { x, result } = await busyRestore(next);
      assert.strictEqual(result, 'ended');
      assert.deepStrictEqual(x.store.ids, []);
      assert.deepStrictEqual(x.r.pendingIds(), []);
    }
  });
});
