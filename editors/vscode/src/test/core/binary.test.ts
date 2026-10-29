// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  available,
  bundledBinaryPath,
  commandLineDir,
  ensureExecutable,
  hostSupported,
  pathAppendValue,
  resolveBinary,
} from '../../core/binary';

describe('binary', () => {
  it('supports macOS and Linux hosts only', () => {
    assert.strictEqual(hostSupported('darwin'), true);
    assert.strictEqual(hostSupported('linux'), true);
    for (const p of ['win32', 'freebsd', 'sunos', 'aix'] as NodeJS.Platform[]) assert.strictEqual(hostSupported(p), false, p);
  });

  it('is unavailable on Windows without running the binary', async () => {
    let ran = false;
    const works = async () => {
      ran = true;
      return true;
    };
    assert.strictEqual(await available('win32', works), false);
    assert.strictEqual(ran, false);
    assert.strictEqual(await available('linux', works), true);
    assert.strictEqual(await available('darwin', async () => false), false);
  });

  it('places the bundled binary and the command-line copy by platform', () => {
    assert.strictEqual(bundledBinaryPath('/ext', 'linux', 'x64'), '/ext/bin/linux-x64/perch');
    assert.strictEqual(commandLineDir('/gs', 'darwin', 'arm64'), '/gs/bin/darwin-arm64');
  });

  it('prefers perch.path and expands ~/', () => {
    const base = { extensionPath: '/ext', platform: 'linux' as const, arch: 'arm64', home: '/home/me' };
    assert.deepStrictEqual(resolveBinary({ ...base, configuredPath: '' }), {
      path: '/ext/bin/linux-arm64/perch',
      source: 'bundled',
    });
    assert.deepStrictEqual(resolveBinary({ ...base, configuredPath: '  ' }).source, 'bundled');
    assert.deepStrictEqual(resolveBinary({ ...base, configuredPath: '/opt/perch' }), {
      path: '/opt/perch',
      source: 'setting',
    });
    assert.strictEqual(resolveBinary({ ...base, configuredPath: '~/bin/perch' }).path, '/home/me/bin/perch');
    assert.strictEqual(resolveBinary({ ...base, configuredPath: '~other/perch' }).path, '~other/perch');
  });

  it('appends to PATH with the delimiter first', () => {
    assert.strictEqual(pathAppendValue('/gs/bin/linux-x64', ':'), ':/gs/bin/linux-x64');
  });

  it('makes a binary executable once', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'perch-bin-'));
    try {
      const file = path.join(dir, 'perch');
      fs.writeFileSync(file, '#!/bin/sh\n', { mode: 0o644 });
      assert.strictEqual(ensureExecutable(file), true);
      assert.strictEqual(fs.statSync(file).mode & 0o777, 0o755);
      assert.strictEqual(ensureExecutable(file), false);
      assert.throws(() => ensureExecutable(path.join(dir, 'missing')));
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
