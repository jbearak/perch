// SPDX-License-Identifier: GPL-3.0-only

// Waiting for a session a terminal is creating: `perch info <id>` reports
// `running` once `exec` succeeded. It fails if the terminal closes first,
// if the session reports an exec error, or after 10 seconds.
import { exitText, Session } from './sessions';

export const READY_TIMEOUT_MS = 10_000;
export const READY_POLL_MS = 100;

export class SessionStartError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'SessionStartError';
  }
}

export interface ReadinessDeps {
  /** `perch info <id>`, or undefined while there is no such session yet. */
  info(): Promise<Session | undefined>;
  /** The terminal's exit code once it has closed: a number, or null if unknown. */
  closed(): number | null | undefined;
  now(): number;
  sleep(ms: number): Promise<void>;
  timeoutMs?: number;
}

/** Resolves with the running session; rejects with a SessionStartError. */
export async function waitUntilRunning(name: string, deps: ReadinessDeps): Promise<Session> {
  const end = deps.now() + (deps.timeoutMs ?? READY_TIMEOUT_MS);
  for (;;) {
    let info: Session | undefined;
    try {
      info = await deps.info();
    } catch {
      // The session directory may be half made; ask again.
    }
    if (info !== undefined) {
      if (info.state === 'running') return info;
      if (info.state === 'exited' && info.exit?.error !== undefined) {
        throw new SessionStartError(`${name} could not start: ${info.exit.error}`);
      }
      if (info.state === 'exited' || info.state === 'lost') {
        // The program ran and has already ended.
        return info;
      }
    }
    const closed = deps.closed();
    if (closed !== undefined) {
      throw new SessionStartError(`${name} could not start: ${closedText(closed)}.`);
    }
    if (deps.now() >= end) {
      throw new SessionStartError(`${name} did not start within ${Math.round((deps.timeoutMs ?? READY_TIMEOUT_MS) / 1000)} seconds.`);
    }
    await deps.sleep(READY_POLL_MS);
  }
}

/** Why a creating terminal closed, from its exit code. */
export function closedText(code: number | null): string {
  if (code === 127) return 'the command was not found (exit code 127)';
  if (code === 126) return 'the command could not be run (exit code 126)';
  return `its terminal closed with ${code === null ? 'an unknown status' : exitText({ code })}`;
}
