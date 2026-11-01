// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { programLaunches, ProgramHost } from '../../core/repls';

function host(executables: string[], o: Partial<ProgramHost> = {}): ProgramHost {
  const set = new Set(executables);
  return {
    env: { PATH: '/opt/bin:/usr/bin' },
    home: '/home/me',
    platform: 'linux',
    isExecutable: (f) => set.has(f),
    exists: () => false,
    ...o,
  };
}

describe('repls', () => {
  it('lists the programs found on PATH, in order, each run bare', () => {
    const list = programLaunches(host(['/usr/bin/node', '/opt/bin/R', '/usr/bin/python3', '/opt/bin/ipython']));
    assert.deepStrictEqual(
      list.map((p) => [p.name, p.kind, p.command]),
      [
        ['Python', 'python', ['/usr/bin/python3']],
        ['IPython', 'python', ['/opt/bin/ipython']],
        ['R', 'r', ['/opt/bin/R']],
        ['Node.js', 'node', ['/usr/bin/node']],
      ],
    );
    assert.deepStrictEqual(programLaunches(host([])), []);
  });

  it('falls back to the second name, and prefers the first', () => {
    assert.deepStrictEqual(programLaunches(host(['/usr/bin/python'])).map((p) => p.display), ['/usr/bin/python']);
    assert.deepStrictEqual(
      programLaunches(host(['/usr/bin/python', '/usr/bin/python3'])).map((p) => p.display),
      ['/usr/bin/python3'],
    );
  });

  it('skips the macOS python3 stub unless the developer tools are installed', () => {
    const mac = { platform: 'darwin' as const };
    assert.deepStrictEqual(programLaunches(host(['/usr/bin/python3'], mac)), []);
    assert.deepStrictEqual(
      programLaunches(host(['/usr/bin/python3', '/opt/bin/python3'], { ...mac, env: { PATH: '/usr/bin:/opt/bin' } })).map((p) => p.display),
      ['/opt/bin/python3'],
      'a later python3 on PATH is used',
    );
    const tools = { ...mac, exists: (f: string) => f === '/Library/Developer/CommandLineTools' };
    assert.deepStrictEqual(programLaunches(host(['/usr/bin/python3'], tools)).map((p) => p.display), ['/usr/bin/python3']);
    assert.deepStrictEqual(programLaunches(host(['/usr/bin/python3'])).map((p) => p.display), ['/usr/bin/python3'], 'Linux has no stub');
  });

  it('escapes braces in the path', () => {
    assert.deepStrictEqual(programLaunches(host(['/o{p}t/julia'], { env: { PATH: '/o{p}t' } }))[0].command, ['/o{{p}}t/julia']);
  });
});
