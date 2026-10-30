// SPDX-License-Identifier: GPL-3.0-only

// Runs the integration tests in a VS Code downloaded by @vscode/test-electron.
// PERCH_VSCODE_VERSION picks the version (default 1.82.0, Perch's minimum).
// Sessions go to a runtime directory of the run's own under /tmp, short
// enough for socket paths, removed afterwards.
//
// Three launches: the main suite, then the cross-process restore in two
// phases that share a user data directory and a workspace, so the second
// window restores from the first one's workspace state. A marker file in
// the shared directory carries the session ID from one phase to the next.
// VS Code keeps workspace storage in memory while it runs extension tests,
// so in those phases the restore set lives in a file there too.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { runTests } from '@vscode/test-electron';
import { bundledBinaryPath } from '../core/binary';

async function main(): Promise<void> {
  const extensionDevelopmentPath = path.resolve(__dirname, '../..');
  const extensionTestsPath = path.resolve(__dirname, 'suite/index');
  const version = process.env.PERCH_VSCODE_VERSION ?? '1.82.0';
  // Short paths under /tmp: VS Code's IPC sockets live in the user data
  // directory, and Perch's in the runtime directory.
  const root = fs.mkdtempSync('/tmp/perch-vsc-');
  const runtimeDir = path.join(root, 'rt');
  fs.mkdirSync(runtimeDir, { mode: 0o700 });
  const run = async (name: string, phase: string | undefined): Promise<void> => {
    const workspace = path.join(root, `ws-${name}`);
    const userData = path.join(root, `ud-${name}`);
    fs.mkdirSync(workspace, { recursive: true });
    await runTests({
      version,
      extensionDevelopmentPath,
      extensionTestsPath,
      extensionTestsEnv: {
        PERCH_RUNTIME_DIR: runtimeDir,
        PERCH_TEST_PHASE: phase ?? '',
        PERCH_TEST_SHARED: root,
        // VS Code keeps workspace storage in memory during extension tests.
        PERCH_TEST_RESTORE_FILE: phase === undefined ? '' : path.join(root, 'restore-set.json'),
      },
      launchArgs: [
        workspace,
        `--user-data-dir=${userData}`,
        '--disable-extensions',
        '--disable-gpu',
        '--skip-welcome',
        '--disable-workspace-trust',
      ],
    });
  };
  try {
    await run('main', undefined);
    await run('restore', 'create');
    await run('restore', 'restore');
  } finally {
    killAll(extensionDevelopmentPath, runtimeDir);
    fs.rmSync(root, { recursive: true, force: true });
  }
}

/** Ends any session a failed test left behind. */
function killAll(extensionPath: string, runtimeDir: string): void {
  const binary = bundledBinaryPath(extensionPath, process.platform, process.arch);
  if (!fs.existsSync(binary)) return;
  const env = { ...process.env, PERCH_RUNTIME_DIR: runtimeDir };
  try {
    const list = JSON.parse(execFileSync(binary, ['list', '--json'], { env, encoding: 'utf8' })) as {
      sessions: { id: string; state: string }[];
    };
    for (const s of list.sessions) {
      if (s.state === 'exited' || s.state === 'lost') continue;
      console.error(`Killing leftover session ${s.id}`);
      execFileSync(binary, ['kill', '--force', s.id], { env });
    }
  } catch (err) {
    console.error('Could not clean up sessions:', err);
  }
}

main().catch((err: unknown) => {
  console.error('VS Code tests failed:', err);
  process.exit(1);
});
