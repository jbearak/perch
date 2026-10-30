// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import {
  attachArgs,
  isSessionId,
  newArgs,
  newSessionId,
  recognize,
  scrollbackLines,
  shellCommand,
} from '../../core/launch';

describe('launch', () => {
  it('makes session IDs perch new --id takes', () => {
    for (let i = 0; i < 200; i++) assert.ok(isSessionId(newSessionId()));
    // Bytes of 252 and above are skipped, so every character is equally likely.
    const bytes = [255, 252, 0, 35, 36, 71, 1, 2, 3, 4, 5, 6];
    assert.strictEqual(newSessionId(() => Buffer.from(bytes)), 'a9a9bcde');
    assert.strictEqual(isSessionId('ABCDEFGH'), false);
    assert.strictEqual(isSessionId('abc'), false);
  });

  it('builds perch new with the terminal flags first and the command last', () => {
    const args = newArgs({
      id: 'abcd1234',
      label: '0123456789abcdef#01234567',
      scrollback: 5000,
      name: 'thesis',
      kind: 'r',
      command: ['R', '--no-save', '--'],
      cwd: '/home/me/thesis',
      env: { R_LIBS: '{dir}/lib' },
      annotations: { project: 'thesis' },
      owner: 'jbearak.raven',
      linkEnv: ['SSH_AUTH_SOCK'],
    });
    assert.deepStrictEqual(args, [
      'new', '--id', 'abcd1234', '--label', '0123456789abcdef#01234567', '--scrollback', '5000',
      '--detach-key', 'none', '--restore', 'full', '--name', 'thesis', '--kind', 'r',
      '--owner', 'jbearak.raven', '--cwd', '/home/me/thesis', '--annotate', 'project=thesis',
      '--env', 'R_LIBS={dir}/lib', '--link-env', 'SSH_AUTH_SOCK', '--', 'R', '--no-save', '--',
    ]);
    assert.ok(!newArgs({ id: 'abcd1234', label: 'l', scrollback: 1, name: '', kind: '', command: ['sh'] }).includes('--link-env'));
  });

  it('builds perch attach for open and for restore', () => {
    assert.deepStrictEqual(attachArgs('abcd1234', 'none#01234567', 1000), [
      'attach', 'abcd1234', '--label', 'none#01234567', '--restore', 'full', '--detach-key', 'none', '--scrollback', '1000',
    ]);
    assert.strictEqual(attachArgs('abcd1234', 'l', 1000, true).at(-1), '--if-detached');
  });

  it('recognizes Perch terminals from their creation options', () => {
    const n = newArgs({ id: 'abcd1234', label: 'l', scrollback: 1, name: 'x', kind: 'shell', command: ['zsh'] });
    assert.deepStrictEqual(recognize('/ext/bin/linux-x64/perch', n), {
      id: 'abcd1234',
      mode: 'new',
      ifDetached: false,
      label: 'l',
    });
    // The path may point at an extension directory an update has deleted.
    assert.deepStrictEqual(recognize('/gone/old-version/bin/perch', attachArgs('zzzz9999', 'l', 1, true)), {
      id: 'zzzz9999',
      mode: 'attach',
      ifDetached: true,
      label: 'l',
    });
    // --if-detached after `--` belongs to the command.
    assert.strictEqual(recognize('perch', ['attach', 'abcd1234', '--', '--if-detached'])?.ifDetached, false);
    assert.strictEqual(recognize('perch', ['attach', 'abcd1234', '--', '--label', 'x'])?.label, undefined);
    assert.strictEqual(recognize('/bin/zsh', n), undefined);
    assert.strictEqual(recognize('/usr/bin/perch-other', n), undefined);
    assert.strictEqual(recognize('/usr/bin/perch', 'new --id abcd1234'), undefined);
    assert.strictEqual(recognize('/usr/bin/perch', ['list']), undefined);
    assert.strictEqual(recognize('/usr/bin/perch', ['new', '--name', 'x', '--id', 'abcd1234']), undefined);
    assert.strictEqual(recognize('/usr/bin/perch', ['attach', 'NOTANID!']), undefined);
    assert.strictEqual(recognize(undefined, undefined), undefined);
  });

  it('passes terminal.integrated.scrollback, clamped', () => {
    assert.strictEqual(scrollbackLines(5000), 5000);
    assert.strictEqual(scrollbackLines(100000), 10000);
    assert.strictEqual(scrollbackLines(12.7), 12);
    assert.strictEqual(scrollbackLines(-1), 1000);
    assert.strictEqual(scrollbackLines('lots'), 1000);
    assert.strictEqual(scrollbackLines(undefined), 1000);
  });

  it('picks the persistent shell', () => {
    assert.deepStrictEqual(shellCommand('', 'darwin', '/bin/zsh'), ['/bin/zsh', '-l']);
    assert.deepStrictEqual(shellCommand('', 'linux', '/bin/bash'), ['/bin/bash']);
    assert.deepStrictEqual(shellCommand('', 'linux', undefined), ['/bin/sh']);
    assert.deepStrictEqual(shellCommand(' /usr/bin/fish ', 'darwin', '/bin/zsh'), ['/usr/bin/fish']);
    assert.deepStrictEqual(shellCommand(['/bin/bash', '--login'], 'linux', '/bin/sh'), ['/bin/bash', '--login']);
    assert.deepStrictEqual(shellCommand([], 'linux', '/bin/bash'), ['/bin/bash']);
  });
});
