// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import {
  API_VERSIONS,
  apiError,
  EndReporter,
  EndReporterDeps,
  errorCode,
  matchesFilter,
  PerchApiError,
  runtimeDir,
  sendErrorCode,
  startingInfo,
  toSessionInfo,
} from '../../core/api';
import { annotateArgs, PerchError, sendArgs } from '../../core/cli';
import { CloseOutcome } from '../../core/restore';
import { diffSessions, Session } from '../../core/sessions';

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'abcd1234',
    name: 'thesis',
    kind: 'python',
    owner: 'example.ext',
    annotations: { 'example.socket': 'ipc' },
    dir: '/tmp/perch-501/abcd1234',
    degraded: [],
    cwd: '/home/me/thesis',
    host: 'login2',
    createdAt: '2026-10-29T15:04:05Z',
    state: 'running',
    attached: true,
    clientLabel: 'aaaaaaaaaaaaaaaa#11111111',
    ...over,
  };
}

const failed = (exitCode: number | undefined, stderr: string, timedOut = false, spawnError?: string) =>
  new PerchError(`perch send: ${stderr}`, exitCode, stderr, timedOut, spawnError);

describe('API core', () => {
  it('serves version 1 only', () => {
    assert.deepStrictEqual(API_VERSIONS, [1]);
  });

  it('copies a session into the shape callers see', () => {
    const s = session({ memoryBytes: 42, lastOutputAt: 'x', exit: { code: 3, signalNumber: 9, error: 'e' } });
    const info = toSessionInfo(s, true);
    assert.deepStrictEqual(info, {
      id: 'abcd1234',
      name: 'thesis',
      kind: 'python',
      owner: 'example.ext',
      annotations: { 'example.socket': 'ipc' },
      dir: '/tmp/perch-501/abcd1234',
      degraded: [],
      cwd: '/home/me/thesis',
      host: 'login2',
      createdAt: '2026-10-29T15:04:05Z',
      state: 'running',
      attached: true,
      attachedHere: true,
      lastOutputAt: 'x',
      memoryBytes: 42,
      exit: { code: 3 },
    });
    info.annotations.x = 'y';
    assert.strictEqual(s.annotations.x, undefined, 'a caller cannot change Perch’s copy');
    assert.strictEqual(toSessionInfo(session({ attached: false }), true).attachedHere, false);
  });


  it('finds the runtime directory as the binary does', () => {
    assert.strictEqual(runtimeDir({}, 501, '/'), '/tmp/perch-501');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: '' }, 501, '/'), '/tmp/perch-501');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: '/run/p' }, 501, '/'), '/run/p');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: 'rel' }, 501, '/home/me'), '/home/me/rel');
  });

  it('describes a session a terminal is still creating', () => {
    const info = startingInfo('abcd1234', '/tmp/perch-501', {
      name: 'python',
      kind: 'python',
      owner: 'example.ext',
      annotations: { a: 'b' },
    });
    assert.strictEqual(info.state, 'starting');
    assert.strictEqual(info.dir, '/tmp/perch-501/abcd1234');
    assert.strictEqual(info.owner, 'example.ext');
    assert.deepStrictEqual(info.annotations, { a: 'b' });
    assert.strictEqual(startingInfo('abcd1234', '/r').owner, undefined);
  });

  it('filters by owner and kind', () => {
    const info = toSessionInfo(session(), false);
    assert.ok(matchesFilter(info, undefined));
    assert.ok(matchesFilter(info, { owner: 'example.ext' }));
    assert.ok(matchesFilter(info, { owner: 'example.ext', kind: 'python' }));
    assert.ok(!matchesFilter(info, { owner: 'other.ext' }));
    assert.ok(!matchesFilter(info, { kind: 'node' }));
  });

  describe('send error codes', () => {
    it('queue_full: exit 3, or over what the queue holds', () => {
      assert.strictEqual(sendErrorCode(failed(3, 'perch: the input queue of session x is full; nothing was sent')), 'queue_full');
      assert.strictEqual(
        sendErrorCode(failed(1, 'perch: the input is over the 1 MiB limit; nothing was sent')),
        'queue_full',
      );
    });

    it('outcome_unknown: exit 4, or cut off by the time limit', () => {
      assert.strictEqual(sendErrorCode(failed(4, 'perch: the connection ... may not have been sent')), 'outcome_unknown');
      assert.strictEqual(sendErrorCode(failed(undefined, '', true)), 'outcome_unknown');
    });

    it('no_session: missing or ended', () => {
      assert.strictEqual(sendErrorCode(failed(1, 'perch: no session `zzzzzzzz`\n')), 'no_session');
      assert.strictEqual(
        sendErrorCode(failed(1, 'perch: session abcd1234 has ended (exited); nothing was sent\n')),
        'no_session',
      );
    });

    it('unavailable: the binary does not run, or the session cannot take it', () => {
      assert.strictEqual(sendErrorCode(failed(undefined, '', false, 'ENOENT')), 'unavailable');
      assert.strictEqual(sendErrorCode(failed(1, 'perch: cannot reach session x: refused; nothing was sent')), 'unavailable');
      assert.strictEqual(sendErrorCode(new Error('other')), 'unavailable');
    });

    it('keeps the message and sets the code', () => {
      const e = apiError(failed(3, 'full'), 'queue_full');
      assert.ok(e instanceof PerchApiError);
      assert.strictEqual((e as PerchApiError).code, 'queue_full');
      assert.strictEqual(e.message, 'perch send: full');
    });
  });

  it('gives other calls a code only where one fits', () => {
    assert.strictEqual(errorCode(failed(1, 'perch: no session `x`')), 'no_session');
    // What `perch kill` says for a session that already ended.
    assert.strictEqual(
      errorCode(failed(1, 'perch: session abcd1234 has already ended (exited); `perch dismiss abcd1234` removes it')),
      'no_session',
    );
    assert.strictEqual(errorCode(failed(undefined, '', false, 'EACCES')), 'unavailable');
    assert.strictEqual(errorCode(failed(1, 'perch: something else')), undefined);
    const plain = new Error('x');
    assert.strictEqual(apiError(plain, undefined), plain);
  });

  it('builds send and annotate command lines', () => {
    assert.deepStrictEqual(sendArgs('abcd1234', {}), ['send', 'abcd1234', '--stdin']);
    assert.deepStrictEqual(sendArgs('abcd1234', { paste: true, submit: true }), [
      'send',
      'abcd1234',
      '--stdin',
      '--paste',
      '--submit',
    ]);
    assert.deepStrictEqual(annotateArgs('abcd1234', { a: '1', 'example.x': 'v=w' }, ['b']), [
      'annotate',
      'abcd1234',
      'a=1',
      'example.x=v=w',
      '--unset',
      'b',
    ]);
    for (const bad of ['', 'a=b', '--unset', '-x']) {
      assert.throws(() => annotateArgs('abcd1234', { [bad]: 'v' }, []), TypeError);
      assert.throws(() => annotateArgs('abcd1234', {}, [bad]), TypeError);
    }
  });

  describe('reporting ends', () => {
    /** `perch info` calls, and a reporter whose `perch info` gives `info` (or throws, for an Error). */
    function reporter(info: Session | undefined | Error = undefined, over: Partial<EndReporterDeps> = {}) {
      const calls: string[] = [];
      const r = new EndReporter({
        info: (id) => {
          calls.push(id);
          return info instanceof Error ? Promise.reject(info) : Promise.resolve(info);
        },
        find: () => undefined,
        now: () => 0,
        ...over,
      });
      return { r, calls };
    }
    /** This window's terminal of the session ended with its process. */
    const exited = (code?: number, over: Partial<CloseOutcome> = {}): CloseOutcome => ({
      id: 'abcd1234',
      reason: 'process',
      code,
      result: 'forget',
      ...over,
    });
    const unreachable = new Error('perch info did not answer');

    it('reports each end once, with the tombstone’s exit', () => {
      const { r } = reporter();
      const live = session();
      const dead = session({ state: 'exited', attached: false, exit: { code: 2 } });
      const first = r.ended(diffSessions([live], [dead]).ended);
      assert.deepStrictEqual(
        first.map((s) => [s.id, s.exit]),
        [['abcd1234', { code: 2 }]],
      );
      assert.deepStrictEqual(r.ended(diffSessions([live], [dead]).ended), []);
    });

    it('a session gone without a tombstone takes the exit this window’s terminal received', async () => {
      const { r } = reporter(unreachable);
      assert.strictEqual(await r.closed(exited(7)), undefined);
      const [s] = r.ended(diffSessions([session()], []).ended);
      assert.strictEqual(s.state, 'exited');
      assert.deepStrictEqual(s.exit, { code: 7 });
    });

    it('and with no terminal there, its exit status is unknown', () => {
      const { r } = reporter();
      const [s] = r.ended(diffSessions([session()], []).ended);
      assert.strictEqual(s.exit, undefined);
    });

    it('a terminal closed by the user, by the extension, or by a busy attach is no end', async () => {
      const { r, calls } = reporter();
      r.attached(session());
      assert.strictEqual(await r.closed(exited(undefined, { reason: 'user' })), undefined);
      assert.strictEqual(await r.closed(exited(undefined, { reason: 'extension' })), undefined);
      assert.strictEqual(await r.closed(exited(75, { result: 'busy' })), undefined);
      assert.deepStrictEqual(calls, [], 'perch info is not asked');
    });

    it('a session perch info shows live (detached, or taken over) is no end', async () => {
      const { r } = reporter(session({ clientLabel: 'fedcba9876543210#bbbbbbbb' }));
      r.attached(session());
      assert.strictEqual(await r.closed(exited(0)), undefined);
      assert.deepStrictEqual(r.reconcile([]), [], 'nothing left pending');
    });

    it('a confirmed end seen through this window’s terminal is reported once, whichever path sees it first', async () => {
      const { r } = reporter();
      // Never listed: known only from its attach.
      r.attached(session());
      const s = await r.closed(exited(5));
      assert.strictEqual(s?.state, 'exited');
      assert.strictEqual(s?.attached, false);
      assert.deepStrictEqual(s?.exit, { code: 5 });
      assert.deepStrictEqual(r.ended(diffSessions([session()], []).ended), [], 'a later listing does not repeat it');
      assert.strictEqual(await r.closed(exited(5)), undefined);
      // The listing first, then the terminal.
      const { r: r2 } = reporter(undefined, { find: () => session() });
      assert.strictEqual(r2.ended(diffSessions([session()], []).ended).length, 1);
      assert.strictEqual(await r2.closed(exited()), undefined);
      // A tombstone's own exit wins; nothing known means nothing to report.
      const tomb = session({ state: 'exited', attached: false, exit: { signal: 'SIGKILL' } });
      assert.deepStrictEqual((await reporter(tomb).r.closed(exited(9)))?.exit, { signal: 'SIGKILL' });
      assert.strictEqual(await reporter().r.closed(exited()), undefined);
    });

    it('an end whose confirmation failed is reported once by the next successful listing without the session', async () => {
      const { r } = reporter(unreachable);
      // Created and exited between listings; `perch info` failed once.
      r.attached(session());
      await r.closed(exited());
      const [s, ...rest] = r.reconcile([]);
      assert.deepStrictEqual(rest, []);
      assert.strictEqual(s.id, 'abcd1234');
      assert.strictEqual(s.state, 'exited');
      assert.strictEqual(s.exit, undefined, 'exit status unknown');
      assert.deepStrictEqual(r.reconcile([]), [], 'once');
      assert.deepStrictEqual(r.ended(diffSessions([session()], []).ended), [], 'and not again by a diff');
    });

    it('with the exit this window’s terminal received, or the tombstone’s', async () => {
      const { r } = reporter(unreachable, { find: () => session() });
      await r.closed(exited(5));
      assert.deepStrictEqual(r.reconcile([])[0].exit, { code: 5 });
      const { r: r2 } = reporter(unreachable, { find: () => session() });
      await r2.closed(exited());
      const tomb = session({ state: 'exited', attached: false, exit: { code: 2 } });
      assert.deepStrictEqual(r2.reconcile([tomb])[0].exit, { code: 2 });
    });

    it('a listing that shows it running (taken over) reports nothing and drops it', async () => {
      const { r } = reporter(unreachable, { find: () => session() });
      await r.closed(exited());
      assert.deepStrictEqual(r.reconcile([session({ clientLabel: 'fedcba9876543210#bbbbbbbb' })]), []);
      assert.deepStrictEqual(r.reconcile([]), [], 'dropped: its later end is a listing’s to report');
    });

    it('a listing started before the end was left pending does not decide it', async () => {
      const { r } = reporter(unreachable, { find: () => session(), now: () => 100 });
      await r.closed(exited());
      // Asked for before: it may predate the session or its takeover.
      assert.deepStrictEqual(r.reconcile([], 50), []);
      assert.deepStrictEqual(r.reconcile([], 100), []);
      // Asked for after: absent means ended.
      assert.strictEqual(r.reconcile([], 150).length, 1);
    });

    it('an unresponsive session stays pending', async () => {
      const { r } = reporter(unreachable, { find: () => session() });
      await r.closed(exited());
      assert.deepStrictEqual(r.reconcile([session({ state: 'unresponsive' })]), []);
      assert.strictEqual(r.reconcile([]).length, 1);
    });

    it('does not report sessions already ended at the first listing', () => {
      const { r } = reporter();
      assert.deepStrictEqual(r.ended(diffSessions([], [session({ state: 'lost' })]).ended), []);
    });
  });
});
