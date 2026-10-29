// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { execFile } from 'child_process';
import * as fs from 'fs';
import { promisify } from 'util';
import * as vscode from 'vscode';
import { bundledBinaryPath } from '../../core/binary';

const extensionId = 'jbearak.perch';

describe('activation', () => {
  it('activates as a workspace extension', async () => {
    const ext = vscode.extensions.getExtension(extensionId);
    assert.ok(ext, `${extensionId} is installed`);
    await ext.activate();
    assert.strictEqual(ext.isActive, true);
    // Without a remote, VS Code runs every extension as UI; the manifest is
    // what puts Perch on the workspace host.
    assert.deepStrictEqual(ext.packageJSON.extensionKind, ['workspace']);
  });

  it('runs the bundled binary', async function () {
    const ext = vscode.extensions.getExtension(extensionId);
    assert.ok(ext);
    const binary = bundledBinaryPath(ext.extensionPath, process.platform, process.arch);
    if (!fs.existsSync(binary)) {
      // CI places the Linux release binary here and sets PERCH_REQUIRE_BINARY.
      if (process.env.PERCH_REQUIRE_BINARY === '1') assert.fail(`missing ${binary}`);
      this.skip();
    }
    const { stdout } = await promisify(execFile)(binary, ['version', '--json']);
    const report = JSON.parse(stdout);
    assert.strictEqual(report.selftest.ok, true, stdout);
  });
});
