// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { workspaceKey } from '../../core/labels';
import { activated, bundledBinaryOrSkip, extensionId, perch } from './helpers';

describe('activation', () => {
  it('activates as a workspace extension', async () => {
    const { ext } = await activated();
    assert.strictEqual(ext.isActive, true);
    // Without a remote, VS Code runs every extension as UI; the manifest is
    // what puts Perch on the workspace host.
    assert.deepStrictEqual(ext.packageJSON.extensionKind, ['workspace']);
    assert.strictEqual(ext.id, extensionId);
  });

  it('labels its client with the workspace key and a window key', async () => {
    const { testing } = await activated();
    const folder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(folder, 'the tests run in a workspace');
    assert.strictEqual(testing.label.split('#')[0], workspaceKey(folder.uri.toString()));
    assert.match(testing.label, /^[0-9a-f]{16}#[0-9a-f]{8}$/);
  });

  it('makes the bundled binary executable and runs it', async function () {
    const { ext } = await activated();
    const binary = bundledBinaryOrSkip(this, ext.extensionPath);
    // Activation restores the bit a registry's archive may drop.
    assert.strictEqual(fs.statSync(binary).mode & 0o777, 0o755);
    const report = JSON.parse(await perch(binary, ['version', '--json']));
    assert.strictEqual(report.selftest.ok, true);
  });

  it('is available here and not on a Windows host', async function () {
    const { ext, testing } = await activated();
    assert.strictEqual(await testing.available('win32'), false);
    bundledBinaryOrSkip(this, ext.extensionPath);
    assert.strictEqual(await testing.available(), true);
  });

  it('installs the command-line copy and puts it on PATH for terminals', async function () {
    const { ext, testing } = await activated();
    bundledBinaryOrSkip(this, ext.extensionPath);
    await testing.commandLineInstalled;
    const copy = path.join(testing.commandLineDir, 'perch');
    assert.ok(fs.existsSync(copy), `${copy} exists`);
    assert.match(testing.commandLineDir, new RegExp(`bin/${process.platform}-${process.arch}$`));
    const report = JSON.parse(await perch(copy, ['version', '--json']));
    assert.strictEqual(report.selftest.ok, true);
    const bin = testing.environment.get('PERCH_BIN');
    assert.strictEqual(bin?.type, vscode.EnvironmentVariableMutatorType.Replace);
    assert.strictEqual(bin?.value, copy);
    const pathMutator = testing.environment.get('PATH');
    assert.strictEqual(pathMutator?.type, vscode.EnvironmentVariableMutatorType.Append);
    assert.strictEqual(pathMutator?.value, `${path.delimiter}${testing.commandLineDir}`);
  });
});
