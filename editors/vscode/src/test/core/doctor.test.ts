// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { doctorWarnedKey, doctorWarning, parseDoctor } from '../../core/doctor';

const macos = JSON.stringify({
  schema: 1,
  ok: true,
  runtimeDir: '/tmp/perch-501',
  survivesLogout: true,
  checks: [{ name: 'host', status: 'info', message: 'macOS has no logind' }],
});

const killUserProcesses = JSON.stringify({
  schema: 1,
  ok: true,
  runtimeDir: '/tmp/perch-1000',
  survivesLogout: false,
  checks: [
    { name: 'runtime-dir', status: 'ok', message: 'fine' },
    { name: 'logind', status: 'warn', message: 'KillUserProcesses=yes: logind ends every process of a login at logout' },
  ],
});

describe('perch doctor', () => {
  it('parses the report', () => {
    const r = parseDoctor(macos);
    assert.strictEqual(r.ok, true);
    assert.strictEqual(r.survivesLogout, true);
    assert.strictEqual(r.runtimeDir, '/tmp/perch-501');
    assert.deepStrictEqual(r.checks, [{ name: 'host', status: 'info', message: 'macOS has no logind' }]);
    assert.strictEqual(parseDoctor(JSON.stringify({ schema: 1, survivesLogout: null })).survivesLogout, null);
    assert.throws(() => parseDoctor('{"schema":2}'));
  });

  it('warns when sessions will not survive logout, naming the reasons', () => {
    assert.strictEqual(
      doctorWarning(parseDoctor(killUserProcesses), 'login2'),
      'Perch sessions on login2 are not expected to survive logout. ' +
        'KillUserProcesses=yes: logind ends every process of a login at logout. Run `perch doctor` for details.',
    );
  });

  it('warns when a check fails', () => {
    const r = parseDoctor(
      JSON.stringify({ schema: 1, ok: false, survivesLogout: true, checks: [{ name: 'flock', status: 'fail', message: 'flock does not work.' }] }),
    );
    assert.strictEqual(doctorWarning(r, 'h'), 'Perch found a problem on h. flock does not work. Run `perch doctor` for details.');
  });

  it('stays quiet when sessions survive, or when Perch cannot tell', () => {
    assert.strictEqual(doctorWarning(parseDoctor(macos), 'h'), undefined);
    assert.strictEqual(doctorWarning(parseDoctor(JSON.stringify({ schema: 1, ok: true, survivesLogout: null })), 'h'), undefined);
  });

  it('remembers the warning per host', () => {
    assert.notStrictEqual(doctorWarnedKey('login1'), doctorWarnedKey('login2'));
  });
});
