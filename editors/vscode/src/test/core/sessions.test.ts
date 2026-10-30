// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import {
  countDetached,
  describe as describeSession,
  diffSessions,
  exitText,
  iconName,
  isEmptyDiff,
  killConfirmation,
  memoryText,
  parseList,
  Session,
  shortAge,
  stateText,
  stripAnsi,
} from '../../core/sessions';

const now = Date.parse('2026-10-29T18:04:05Z');
const label = '0123456789abcdef#deadbeef';
const ctx = { now, home: '/home/me', label };

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'k3x9q2ab',
    name: 'R: thesis',
    kind: 'r',
    annotations: {},
    dir: '/tmp/perch-501/k3x9q2ab',
    degraded: [],
    cwd: '/home/me/thesis',
    host: 'login2',
    createdAt: '2026-10-29T15:04:05Z',
    state: 'running',
    attached: false,
    ...over,
  };
}

describe('sessions', () => {
  it('parses list --json, as the CLI golden file has it', () => {
    const stdout = JSON.stringify({
      schema: 1,
      sessions: [
        {
          annotations: { 'raven.console': 'ipc', bad: 3 },
          attached: false,
          clientLabel: null,
          cols: 80,
          command: ['sleep', '100'],
          createdAt: '2026-10-29T15:04:05Z',
          cwd: '/home/me',
          degraded: [],
          dir: '/tmp/perch-501/aaaaaaaa',
          exit: null,
          format: 1,
          host: 'login2',
          id: 'aaaaaaaa',
          kind: '',
          lastOutputAt: null,
          name: 'a running one',
          owner: 'jbearak.raven',
          state: 'running',
          somethingNew: true,
        },
        {
          annotations: {},
          attached: false,
          createdAt: '2026-10-29T15:04:05Z',
          cwd: '/home/me',
          degraded: ['directory-recreated'],
          dir: '/tmp/perch-501/bbbbbbbb',
          exit: { code: 7 },
          id: 'bbbbbbbb',
          kind: 'shell',
          name: 'b exited one',
          state: 'exited',
        },
        { id: 'cccccccc', state: 'confused' },
        { state: 'running' },
      ],
    });
    const list = parseList(stdout);
    assert.strictEqual(list.length, 2);
    assert.deepStrictEqual(list[0].annotations, { 'raven.console': 'ipc' });
    assert.strictEqual(list[0].owner, 'jbearak.raven');
    assert.strictEqual(list[0].clientLabel, undefined);
    assert.strictEqual(list[0].exit, undefined);
    assert.deepStrictEqual(list[1].exit, { code: 7 });
    assert.deepStrictEqual(list[1].degraded, ['directory-recreated']);
  });

  it('refuses another schema', () => {
    assert.throws(() => parseList('{"schema":2,"sessions":[]}'));
    assert.throws(() => parseList('not json'));
  });

  it('words exits as the CLI does', () => {
    assert.strictEqual(exitText({ code: 0 }), 'exit code 0');
    assert.strictEqual(exitText({ signal: 'SIGKILL', signalNumber: 9 }), 'killed by signal 9 (SIGKILL)');
    assert.strictEqual(exitText({ signal: 'SIGKILL' }), 'killed by SIGKILL');
    assert.strictEqual(exitText({ error: 'cannot run foo' }), 'cannot run foo');
    assert.strictEqual(exitText(undefined), 'exit status unknown');
  });

  it('formats ages and memory', () => {
    assert.deepStrictEqual([5, 180, 7200, 200_000, -3].map(shortAge), ['5s', '3m', '2h', '2d', '0s']);
    assert.strictEqual(memoryText(4_100_000_000), 'about 4.1 GB');
    assert.strictEqual(memoryText(512), 'about 512 bytes');
    assert.strictEqual(memoryText(123_456_789), 'about 123 MB');
  });

  it('describes a session for the list', () => {
    assert.strictEqual(describeSession(session({ memoryBytes: 4_100_000_000 }), ctx), 'R · ~/thesis · 3h · about 4.1 GB · detached');
    assert.strictEqual(
      describeSession(session({ kind: '', cwd: '/srv/x', attached: true, clientLabel: 'perch-cli' }), ctx),
      'Program · /srv/x · 3h · attached',
    );
    assert.strictEqual(describeSession(session({ attached: true, clientLabel: label }), ctx), 'R · ~/thesis · 3h · attached here');
    assert.strictEqual(
      describeSession(session({ state: 'exited', exit: { code: 1 }, degraded: ['directory-recreated'] }), ctx),
      'R · ~/thesis · 3h · exited, exit code 1 · warning: directory-recreated',
    );
    assert.strictEqual(stateText(session({ state: 'lost' }), label), 'lost');
    assert.strictEqual(stateText(session({ state: 'unresponsive' }), label), 'unresponsive');
  });

  it('picks icons by kind, then by state', () => {
    assert.strictEqual(iconName(session()), 'graph');
    assert.strictEqual(iconName(session({ kind: 'shell' })), 'terminal');
    assert.strictEqual(iconName(session({ kind: 'julia' })), 'window');
    assert.strictEqual(iconName(session({ state: 'exited' })), 'circle-slash');
    assert.strictEqual(iconName(session({ state: 'lost' })), 'warning');
    assert.strictEqual(iconName(session({ degraded: ['directory-recreated'] })), 'warning');
  });

  it('confirms a kill with name, kind, directory, age and memory', () => {
    const c = killConfirmation(session({ memoryBytes: 2_000_000 }), ctx);
    assert.strictEqual(c.message, 'Kill R: thesis?');
    assert.match(c.detail, /Kind: R\n/);
    assert.match(c.detail, /Working directory: ~\/thesis\n/);
    assert.match(c.detail, /Age: 3h\n/);
    assert.match(c.detail, /Memory: about 2.0 MB/);
    assert.match(killConfirmation(session(), ctx).detail, /Memory: unknown/);
    assert.match(killConfirmation(session({ attached: true, clientLabel: 'x' }), ctx).detail, /another window/);
    assert.strictEqual(killConfirmation(session({ name: '' }), ctx).message, 'Kill k3x9q2ab?');
  });

  it('counts detached running sessions', () => {
    const list = [
      session({ id: 'a' }),
      session({ id: 'b', attached: true }),
      session({ id: 'c', state: 'exited' }),
      session({ id: 'd', state: 'unresponsive' }),
      session({ id: 'e' }),
    ];
    assert.strictEqual(countDetached(list), 2);
  });

  describe('diffs', () => {
    it('finds additions, changes and removals', () => {
      const a = session({ id: 'a' });
      const b = session({ id: 'b' });
      const d = diffSessions([a, b], [{ ...a, attached: true }, session({ id: 'c' })]);
      assert.deepStrictEqual(d.added.map((s) => s.id), ['c']);
      assert.deepStrictEqual(d.changed.map((s) => s.id), ['a']);
      assert.deepStrictEqual(d.removed.map((s) => s.id), ['b']);
      assert.ok(isEmptyDiff(diffSessions([a, b], [{ ...a }, { ...b }])));
      assert.ok(!isEmptyDiff(diffSessions([a], [{ ...a, name: 'renamed' }])));
    });

    it('counts a changed annotation set or value as a change, and an equal one as none', () => {
      const a = session({ id: 'a', annotations: { k: 'v', x: 'y' } });
      assert.ok(isEmptyDiff(diffSessions([a], [{ ...a, annotations: { x: 'y', k: 'v' } }])), 'order does not matter');
      const variants: Record<string, string>[] = [{ k: 'w', x: 'y' }, { k: 'v' }, { k: 'v', x: 'y', z: '' }];
      for (const annotations of variants) {
        const d = diffSessions([a], [{ ...a, annotations }]);
        assert.deepStrictEqual(d.changed.map((s) => s.id), ['a'], JSON.stringify(annotations));
        assert.deepStrictEqual(d.ended, []);
      }
    });

    it('reports a session that ended with its tombstone', () => {
      const a = session({ id: 'a' });
      const d = diffSessions([a], [{ ...a, state: 'exited', exit: { code: 0 } }]);
      assert.deepStrictEqual(d.ended.map((s) => [s.id, s.exit]), [['a', { code: 0 }]]);
    });

    it('reports a session that vanished as ended with an unknown status', () => {
      const d = diffSessions([session({ id: 'a' })], []);
      assert.strictEqual(d.ended.length, 1);
      assert.strictEqual(d.ended[0].state, 'exited');
      assert.strictEqual(exitText(d.ended[0].exit), 'exit status unknown');
    });

    it('does not report a dismissed tombstone as ending again', () => {
      const t = session({ id: 't', state: 'exited', exit: { code: 0 } });
      const d = diffSessions([t], []);
      assert.deepStrictEqual(d.removed.map((s) => s.id), ['t']);
      assert.deepStrictEqual(d.ended, []);
      assert.deepStrictEqual(diffSessions([t], [t]).ended, []);
    });
  });

  it('strips escape sequences from the saved tail', () => {
    const raw = '\x1b[1;31mred\x1b[0m text\r\n\x1b]0;title\x07\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\x1bPq#0\x1b\\\x1b=do\x1b(Bne\n';
    assert.strictEqual(stripAnsi(raw), 'red text\nlinkdone\n');
  });
});
