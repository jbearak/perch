// SPDX-License-Identifier: GPL-3.0-only

// Running the perch binary. Each call has a time limit, and a failure
// carries the command's own message.
import { execFile } from 'child_process';
import { parseList, Session } from './sessions';

export class PerchError extends Error {
  constructor(
    message: string,
    readonly exitCode: number | undefined,
    readonly stderr: string,
  ) {
    super(message);
    this.name = 'PerchError';
  }
}

export interface RunResult {
  stdout: string;
  stderr: string;
}

/** Runs `binary args...` and resolves with its output if it exits 0. */
export function runPerch(binary: string, args: readonly string[], timeoutMs: number): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    execFile(
      binary,
      args,
      { timeout: timeoutMs, maxBuffer: 16 * 1024 * 1024, encoding: 'utf8' },
      (err, stdout, stderr) => {
        if (err === null) {
          resolve({ stdout, stderr });
          return;
        }
        const e = err as NodeJS.ErrnoException & { code?: unknown; killed?: boolean };
        const code = typeof e.code === 'number' ? e.code : undefined;
        const firstLine = stderr.trim().split('\n')[0]?.replace(/^perch: /, '') ?? '';
        const why = e.killed
          ? `timed out after ${timeoutMs / 1000} seconds`
          : firstLine !== ''
            ? firstLine
            : e.message;
        reject(new PerchError(`perch ${args[0] ?? ''}: ${why}`, code, stderr));
      },
    );
  });
}

/** The CLI operations the extension uses. */
export class PerchCli {
  constructor(readonly binary: string) {}

  async list(): Promise<Session[]> {
    const { stdout } = await runPerch(this.binary, ['list', '--json', '--all'], 10_000);
    return parseList(stdout);
  }

  /** Kill waits out the 5-second grace period. */
  async kill(id: string, force = false): Promise<void> {
    await runPerch(this.binary, force ? ['kill', id, '--force'] : ['kill', id], 20_000);
  }

  async dismiss(id: string): Promise<void> {
    await runPerch(this.binary, ['dismiss', id], 10_000);
  }

  /** `perch self-install <dest>`; resolves with the copy's path. */
  async selfInstall(dest: string): Promise<{ path: string; installed: boolean; locked: boolean }> {
    const { stdout } = await runPerch(this.binary, ['self-install', dest], 60_000);
    const r = JSON.parse(stdout) as { path: string; installed: boolean; locked: boolean };
    return { path: r.path, installed: r.installed, locked: r.locked };
  }

  /** Whether the binary runs and passes its selftest. */
  async works(): Promise<boolean> {
    try {
      await runPerch(this.binary, ['version'], 10_000);
      return true;
    } catch {
      return false;
    }
  }
}
