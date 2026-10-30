// SPDX-License-Identifier: GPL-3.0-only

// Running the perch binary. Each call has a time limit, and a failure
// carries the command's own message.
import { execFile, spawnSync } from 'child_process';
import { parseInfo, parseList, Session } from './sessions';

export class PerchError extends Error {
  constructor(
    message: string,
    readonly exitCode: number | undefined,
    readonly stderr: string,
    /** The time limit ended it; it may have done its work. */
    readonly timedOut = false,
    /** It could not be started: `ENOENT`, `EACCES` and the like. */
    readonly spawnError?: string,
    readonly stdout = '',
  ) {
    super(message);
    this.name = 'PerchError';
  }
}

export interface RunResult {
  stdout: string;
  stderr: string;
}

/**
 * Runs `binary args...` and resolves with its output if it exits 0.
 * `input`, if given, is written to its stdin.
 */
export function runPerch(binary: string, args: readonly string[], timeoutMs: number, input?: string): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    const child = execFile(
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
        const spawnError = typeof e.code === 'string' ? e.code : undefined;
        const firstLine = stderr.trim().split('\n')[0]?.replace(/^perch: /, '') ?? '';
        const why = e.killed
          ? `timed out after ${timeoutMs / 1000} seconds`
          : firstLine !== ''
            ? firstLine
            : e.message;
        reject(new PerchError(`perch ${args[0] ?? ''}: ${why}`, code, stderr, e.killed === true, spawnError, stdout));
      },
    );
    if (input !== undefined) {
      // A child that exits before reading all of it closes the pipe; its exit says why.
      child.stdin?.on('error', () => undefined);
      child.stdin?.end(input);
    }
  });
}

/** The CLI operations the extension uses. */
export class PerchCli {
  constructor(readonly binary: string) {}

  async list(): Promise<Session[]> {
    const { stdout } = await runPerch(this.binary, ['list', '--json', '--all'], 10_000);
    return parseList(stdout);
  }

  /** `perch info <id>`, or undefined if there is no such session. */
  async info(id: string): Promise<Session | undefined> {
    try {
      const { stdout } = await runPerch(this.binary, ['info', id, '--json'], 10_000);
      return parseInfo(stdout);
    } catch (err) {
      if (err instanceof PerchError && /^no session /m.test(err.stderr.replace(/^perch: /m, ''))) return undefined;
      throw err;
    }
  }

  /**
   * Whether `perch new` takes `--link-env`. The probe has no command, so it
   * never starts a session: a binary with the flag stops at the missing
   * command, one without it at the flag. It runs synchronously, once per
   * binary, because `prepareLaunch` is synchronous; it takes milliseconds.
   */
  supportsLinkEnv(): boolean {
    const r = spawnSync(this.binary, ['new', '--link-env', 'PATH'], { encoding: 'utf8', timeout: 10_000 });
    return !/--link-env\b.*not supported/.test(r.stderr ?? '');
  }

  /** Kill waits out the 5-second grace period. */
  async kill(id: string, force = false): Promise<void> {
    await runPerch(this.binary, force ? ['kill', id, '--force'] : ['kill', id], 20_000);
  }

  /**
   * `perch send`, the text on stdin: an argument has a length limit, and
   * the queue takes up to 1 MiB. Waits for `queued`.
   */
  async send(id: string, text: string, options: { paste?: boolean; submit?: boolean } = {}): Promise<void> {
    await runPerch(this.binary, sendArgs(id, options), 20_000, text);
  }

  async annotate(id: string, set: Readonly<Record<string, string>>, unset: readonly string[] = []): Promise<void> {
    await runPerch(this.binary, annotateArgs(id, set, unset), 10_000);
  }

  /** `perch doctor --json`, as printed; it exits 1 when a check fails. */
  async doctor(): Promise<string> {
    try {
      return (await runPerch(this.binary, ['doctor', '--json'], 20_000)).stdout;
    } catch (err) {
      if (err instanceof PerchError && err.exitCode === 1 && err.stdout !== '') return err.stdout;
      throw err;
    }
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

/** `perch send <id> --stdin`, with `--paste` and `--submit` as asked. */
export function sendArgs(id: string, options: { paste?: boolean; submit?: boolean }): string[] {
  const args = ['send', id, '--stdin'];
  if (options.paste === true) args.push('--paste');
  if (options.submit === true) args.push('--submit');
  return args;
}

/**
 * `perch annotate <id> K=V... --unset K...`. Keys are non-empty, without
 * `=`, and do not start with `-`, so none reads as an option.
 */
export function annotateArgs(
  id: string,
  set: Readonly<Record<string, string>>,
  unset: readonly string[],
): string[] {
  const args = ['annotate', id];
  for (const [k, v] of Object.entries(set)) {
    checkKey(k);
    if (typeof v !== 'string') throw new TypeError(`annotation ${k}: the value must be a string`);
    args.push(`${k}=${v}`);
  }
  for (const k of unset) {
    checkKey(k);
    args.push('--unset', k);
  }
  return args;
}

function checkKey(k: unknown): void {
  if (typeof k !== 'string' || k === '' || k.includes('=') || k.startsWith('-')) {
    throw new TypeError(`${JSON.stringify(k)} is not an annotation key`);
  }
}
