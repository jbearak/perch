// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { execFile } from 'child_process';
import * as fs from 'fs';
import { promisify } from 'util';
import * as vscode from 'vscode';
import { bundledBinaryPath } from '../../core/binary';
import type { Exports, Testing } from '../../vscode/activation';
import type { CloseOutcome } from '../../vscode/terminals';

export const extensionId = 'jbearak.perch';

export async function activated(): Promise<{ ext: vscode.Extension<Exports>; testing: Testing }> {
  const ext = vscode.extensions.getExtension<Exports>(extensionId);
  assert.ok(ext, `${extensionId} is installed`);
  const api = await ext.activate();
  assert.ok(api.testing, 'test mode exports the testing seams');
  return { ext, testing: api.testing };
}

/**
 * The bundled binary, or a skip when it is missing. CI places the Linux
 * release binary and sets PERCH_REQUIRE_BINARY, which makes a missing one
 * a failure.
 */
export function bundledBinaryOrSkip(ctx: Mocha.Context, extensionPath: string): string {
  const binary = bundledBinaryPath(extensionPath, process.platform, process.arch);
  if (!fs.existsSync(binary)) {
    if (process.env.PERCH_REQUIRE_BINARY === '1') assert.fail(`missing ${binary}`);
    ctx.skip();
  }
  return binary;
}

/** Runs the binary with the test run's PERCH_RUNTIME_DIR. */
export async function perch(binary: string, args: string[]): Promise<string> {
  const { stdout } = await promisify(execFile)(binary, args, { timeout: 30_000 });
  return stdout;
}

export async function waitFor<T>(what: string, f: () => Promise<T | undefined>, timeoutMs = 15_000): Promise<T> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const v = await f();
    if (v !== undefined) return v;
    if (Date.now() > end) assert.fail(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

/** `perch info <id> --json`, or undefined once the session is gone. */
export async function info(binary: string, id: string): Promise<Record<string, unknown> | undefined> {
  try {
    return JSON.parse(await perch(binary, ['info', id, '--json'])) as Record<string, unknown>;
  } catch {
    return undefined;
  }
}

/** Resolves with what the extension made of the next close of a terminal of `id`. */
export function nextClose(testing: Testing, id: string, timeoutMs = 15_000): Promise<CloseOutcome> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error(`timed out waiting for the terminal of ${id} to close`));
    }, timeoutMs);
    const sub = testing.onDidClose((o) => {
      if (o.id !== id) return;
      clearTimeout(timer);
      sub.dispose();
      resolve(o);
    });
  });
}
