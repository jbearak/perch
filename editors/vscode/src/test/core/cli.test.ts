// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { PerchCli } from '../../core/cli';

/** A stand-in `perch` that writes `stderr` and exits 2, like `perch new` does. */
function fakePerch(stderr: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'perch-cli-'));
  const bin = path.join(dir, 'perch');
  fs.writeFileSync(bin, `#!/bin/sh\ncat >&2 <<'END'\n${stderr}\nEND\nexit 2\n`, { mode: 0o755 });
  return bin;
}

describe('PerchCli probes', () => {
  it('a binary that stops at the missing command takes --no-link-env', () => {
    const cli = new PerchCli(fakePerch('perch: missing command: perch new [options] -- <command> [args...]'));
    assert.strictEqual(cli.supportsNoLinkEnv(), true);
  });

  it('a binary from before --no-link-env does not take it', () => {
    const cli = new PerchCli(fakePerch('perch: unknown option `--no-link-env`'));
    assert.strictEqual(cli.supportsNoLinkEnv(), false);
    // It still takes --link-env.
    assert.strictEqual(cli.supportsLinkEnv(), true);
  });
});
