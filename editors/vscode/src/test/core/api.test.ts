// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import {
  API_VERSIONS,
  apiError,
  EndReporter,
  errorCode,
  isAttachedHere,
  matchesFilter,
  PerchApiError,
  runtimeDir,
  sendErrorCode,
  startingInfo,
  toSessionInfo,
} from '../../core/api';
import { annotateArgs, PerchError, sendArgs } from '../../core/cli';
import { diffSessions, Session } from '../../core/sessions';

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'abcd1234',
    name: 'R: thesis',
    kind: 'r',
    owner: 'jbearak.raven',
    annotations: { 'raven.console': 'ipc' },
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
      name: 'R: thesis',
      kind: 'r',
      owner: 'jbearak.raven',
      annotations: { 'raven.console': 'ipc' },
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

  it('counts a session attached by any of this window’s labels as attached here', () => {
    const s = session();
    assert.ok(isAttachedHere(s, ['bbbbbbbbbbbbbbbb#22222222', 'aaaaaaaaaaaaaaaa#11111111']));
    assert.ok(!isAttachedHere(s, ['bbbbbbbbbbbbbbbb#22222222']));
    assert.ok(!isAttachedHere(session({ attached: false }), ['aaaaaaaaaaaaaaaa#11111111']));
  });

  it('finds the runtime directory as the binary does', () => {
    assert.strictEqual(runtimeDir({}, 501, '/'), '/tmp/perch-501');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: '' }, 501, '/'), '/tmp/perch-501');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: '/run/p' }, 501, '/'), '/run/p');
    assert.strictEqual(runtimeDir({ PERCH_RUNTIME_DIR: 'rel' }, 501, '/home/me'), '/home/me/rel');
  });

  it('describes a session a terminal is still creating', () => {
    const info = startingInfo('abcd1234', '/tmp/perch-501', {
      name: 'R',
      kind: 'r',
      owner: 'jbearak.raven',
      annotations: { a: 'b' },
    });
    assert.strictEqual(info.state, 'starting');
    assert.strictEqual(info.dir, '/tmp/perch-501/abcd1234');
    assert.strictEqual(info.owner, 'jbearak.raven');
    assert.deepStrictEqual(info.annotations, { a: 'b' });
    assert.strictEqual(startingInfo('abcd1234', '/r').owner, undefined);
  });

  it('filters by owner and kind', () => {
    const info = toSessionInfo(session(), false);
    assert.ok(matchesFilter(info, undefined));
    assert.ok(matchesFilter(info, { owner: 'jbearak.raven' }));
    assert.ok(matchesFilter(info, { owner: 'jbearak.raven', kind: 'r' }));
    assert.ok(!matchesFilter(info, { owner: 'jbearak.sight' }));
    assert.ok(!matchesFilter(info, { kind: 'stata' }));
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
    assert.deepStrictEqual(annotateArgs('abcd1234', { a: '1', 'raven.x': 'v=w' }, ['b']), [
      'annotate',
      'abcd1234',
      'a=1',
      'raven.x=v=w',
      '--unset',
      'b',
    ]);
    for (const bad of ['', 'a=b', '--unset', '-x']) {
      assert.throws(() => annotateArgs('abcd1234', { [bad]: 'v' }, []), TypeError);
      assert.throws(() => annotateArgs('abcd1234', {}, [bad]), TypeError);
    }
  });

  describe('reporting ends', () => {
    it('reports each end once, with the tombstone’s exit', () => {
      const r = new EndReporter();
      const live = session();
      const dead = session({ state: 'exited', attached: false, exit: { code: 2 } });
      const first = r.ended(diffSessions([live], [dead]).ended);
      assert.deepStrictEqual(
        first.map((s) => [s.id, s.exit]),
        [['abcd1234', { code: 2 }]],
      );
      assert.deepStrictEqual(r.ended(diffSessions([live], [dead]).ended), []);
    });

    it('a session gone without a tombstone takes the exit this window’s terminal received', () => {
      const r = new EndReporter();
      r.terminalExit('abcd1234', 7);
      const [s] = r.ended(diffSessions([session()], []).ended);
      assert.strictEqual(s.state, 'exited');
      assert.deepStrictEqual(s.exit, { code: 7 });
    });

    it('and with no terminal there, its exit status is unknown', () => {
      const r = new EndReporter();
      const [s] = r.ended(diffSessions([session()], []).ended);
      assert.strictEqual(s.exit, undefined);
    });

    it('a confirmed end seen through this window’s terminal is reported once, whichever path sees it first', () => {
      const r = new EndReporter();
      r.terminalExit('abcd1234', 5);
      // Never listed: known only from its attach.
      const s = r.confirmedEnd(undefined, session());
      assert.strictEqual(s?.state, 'exited');
      assert.strictEqual(s?.attached, false);
      assert.deepStrictEqual(s?.exit, { code: 5 });
      assert.deepStrictEqual(r.ended(diffSessions([session()], []).ended), [], 'a later listing does not repeat it');
      assert.strictEqual(r.confirmedEnd(undefined, session()), undefined);
      // The listing first, then the terminal.
      const r2 = new EndReporter();
      assert.strictEqual(r2.ended(diffSessions([session()], []).ended).length, 1);
      assert.strictEqual(r2.confirmedEnd(undefined, session()), undefined);
      // A tombstone's own exit wins; nothing known means nothing to report.
      const tomb = session({ state: 'exited', attached: false, exit: { signal: 'SIGKILL' } });
      assert.deepStrictEqual(new EndReporter().confirmedEnd(tomb, undefined)?.exit, { signal: 'SIGKILL' });
      assert.strictEqual(new EndReporter().confirmedEnd(undefined, undefined), undefined);
    });

    it('an end whose confirmation failed is reported once by the next successful listing without the session', () => {
      const r = new EndReporter();
      // Created and exited between listings; `perch info` failed once.
      r.unconfirmed('abcd1234', session());
      const [s, ...rest] = r.reconcile([]);
      assert.deepStrictEqual(rest, []);
      assert.strictEqual(s.id, 'abcd1234');
      assert.strictEqual(s.state, 'exited');
      assert.strictEqual(s.exit, undefined, 'exit status unknown');
      assert.deepStrictEqual(r.reconcile([]), [], 'once');
      assert.deepStrictEqual(r.ended(diffSessions([session()], []).ended), [], 'and not again by a diff');
    });

    it('with the exit this window’s terminal received, or the tombstone’s', () => {
      const r = new EndReporter();
      r.terminalExit('abcd1234', 5);
      r.unconfirmed('abcd1234', session());
      assert.deepStrictEqual(r.reconcile([])[0].exit, { code: 5 });
      const r2 = new EndReporter();
      r2.unconfirmed('abcd1234', session());
      const tomb = session({ state: 'exited', attached: false, exit: { code: 2 } });
      assert.deepStrictEqual(r2.reconcile([tomb])[0].exit, { code: 2 });
    });

    it('a listing that shows it running (taken over) reports nothing and drops it', () => {
      const r = new EndReporter();
      r.unconfirmed('abcd1234', session());
      assert.deepStrictEqual(r.reconcile([session({ clientLabel: 'fedcba9876543210#bbbbbbbb' })]), []);
      assert.deepStrictEqual(r.reconcile([]), [], 'dropped: its later end is a listing’s to report');
    });

    it('a listing started before the end was left pending does not decide it', () => {
      const r = new EndReporter();
      r.unconfirmed('abcd1234', session(), 100);
      // Asked for before: it may predate the session or its takeover.
      assert.deepStrictEqual(r.reconcile([], 50), []);
      assert.deepStrictEqual(r.reconcile([], 100), []);
      // Asked for after: absent means ended.
      assert.strictEqual(r.reconcile([], 150).length, 1);
    });

    it('an unresponsive session stays pending', () => {
      const r = new EndReporter();
      r.unconfirmed('abcd1234', session());
      assert.deepStrictEqual(r.reconcile([session({ state: 'unresponsive' })]), []);
      assert.strictEqual(r.reconcile([]).length, 1);
    });

    it('does not report sessions already ended at the first listing', () => {
      const r = new EndReporter();
      assert.deepStrictEqual(r.ended(diffSessions([], [session({ state: 'lost' })]).ended), []);
    });
  });
});
