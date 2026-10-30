// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { ReadinessDeps, SessionStartError, waitUntilRunning } from '../../core/readiness';
import { Session } from '../../core/sessions';

function session(over: Partial<Session>): Session {
  return {
    id: 'abcd1234', name: 'x', kind: 'shell', annotations: {}, dir: '/tmp/x', degraded: [], cwd: '/',
    host: 'h', createdAt: '2026-10-29T00:00:00Z', state: 'running', attached: true, ...over,
  };
}

/** A clock that moves with each sleep; `script` answers each info call in turn. */
function deps(script: (Session | undefined | Error)[], closedAt?: { after: number; code: number | null }): ReadinessDeps & { calls: number } {
  let t = 0;
  const d = {
    calls: 0,
    info: async () => {
      const v = script[Math.min(d.calls, script.length - 1)];
      d.calls++;
      if (v instanceof Error) throw v;
      return v;
    },
    closed: () => (closedAt !== undefined && d.calls >= closedAt.after ? closedAt.code : undefined),
    now: () => t,
    sleep: async (ms: number) => {
      t += ms;
    },
  };
  return d;
}

describe('readiness', () => {
  it('waits until the session is running', async () => {
    const d = deps([undefined, new Error('half made'), session({ state: 'starting' }), session({})]);
    const s = await waitUntilRunning('x', d);
    assert.strictEqual(s.state, 'running');
    assert.strictEqual(d.calls, 4);
  });

  it('fails with the exec error the session reports', async () => {
    const d = deps([session({ state: 'exited', exit: { error: 'cannot run `nope`: No such file or directory' } })]);
    await assert.rejects(waitUntilRunning('R', d), (e: Error) => e instanceof SessionStartError && /R could not start: cannot run `nope`/.test(e.message));
  });

  it('fails when the terminal closes first', async () => {
    await assert.rejects(waitUntilRunning('R', deps([undefined], { after: 3, code: 127 })), /R could not start: the command was not found \(exit code 127\)/);
    await assert.rejects(waitUntilRunning('R', deps([undefined], { after: 1, code: 2 })), /its terminal closed with exit code 2/);
    await assert.rejects(waitUntilRunning('R', deps([undefined], { after: 1, code: null })), /an unknown status/);
  });

  it('gives up after 10 seconds', async () => {
    const d = deps([undefined]);
    await assert.rejects(waitUntilRunning('R', d), /R did not start within 10 seconds/);
    assert.ok(d.calls >= 100 && d.calls <= 102, `${d.calls} polls`);
  });

  it('accepts a program that already ended', async () => {
    const s = await waitUntilRunning('x', deps([session({ state: 'exited', exit: { code: 0 } })]));
    assert.strictEqual(s.state, 'exited');
  });
});
