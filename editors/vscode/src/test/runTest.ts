// SPDX-License-Identifier: GPL-3.0-only

// Runs the integration tests in a VS Code downloaded by @vscode/test-electron.
// PERCH_VSCODE_VERSION picks the version (default 1.82.0, Perch's minimum).
// Sessions go to a runtime directory of the run's own under /tmp, short
// enough for socket paths, removed afterwards.
import * as fs from 'fs';
import * as path from 'path';
import { runTests } from '@vscode/test-electron';

async function main(): Promise<void> {
  const extensionDevelopmentPath = path.resolve(__dirname, '../..');
  const extensionTestsPath = path.resolve(__dirname, 'suite/index');
  const version = process.env.PERCH_VSCODE_VERSION ?? '1.82.0';
  const runtimeDir = fs.mkdtempSync('/tmp/perch-vsc-');
  fs.chmodSync(runtimeDir, 0o700);
  try {
    await runTests({
      version,
      extensionDevelopmentPath,
      extensionTestsPath,
      extensionTestsEnv: { PERCH_RUNTIME_DIR: runtimeDir },
      launchArgs: ['--disable-extensions', '--disable-gpu', '--skip-welcome'],
    });
  } finally {
    fs.rmSync(runtimeDir, { recursive: true, force: true });
  }
}

main().catch((err: unknown) => {
  console.error('VS Code tests failed:', err);
  process.exit(1);
});
