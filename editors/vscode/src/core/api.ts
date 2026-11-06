// SPDX-License-Identifier: GPL-3.0-only

// The parts of the API for other extensions that need no VS Code: the
// session shape callers see, the error codes, which versions exist, and
// reporting each session's end once.
import * as path from 'path';
import { PerchError } from './cli';
import { CloseOutcome } from './restore';
import { isEnded, Session, SessionExit } from './sessions';

/** The API versions `getApi` serves. */
export const API_VERSIONS: readonly number[] = [1];

export type SessionInfoState = Session['state'];

/** A session as the API reports it. */
export interface SessionInfo {
  id: string;
  name: string;
  kind: string;
  owner?: string;
  annotations: Record<string, string>;
  /** The session directory ({dir}). */
  dir: string;
  /** For example `directory-recreated`. */
  degraded: string[];
  cwd: string;
  host: string;
  createdAt: string;
  state: SessionInfoState;
  attached: boolean;
  attachedHere: boolean;
  lastOutputAt?: string;
  memoryBytes?: number;
  exit?: { code?: number; signal?: string };
}

/** A copy of a listed session, so callers cannot change Perch's. */
export function toSessionInfo(s: Session, attachedHere: boolean): SessionInfo {
  const info: SessionInfo = {
    id: s.id,
    name: s.name,
    kind: s.kind,
    annotations: { ...s.annotations },
    dir: s.dir,
    degraded: [...s.degraded],
    cwd: s.cwd,
    host: s.host,
    createdAt: s.createdAt,
    state: s.state,
    attached: s.attached,
    attachedHere: s.attached && attachedHere,
  };
  if (s.owner !== undefined) info.owner = s.owner;
  if (s.lastOutputAt !== undefined) info.lastOutputAt = s.lastOutputAt;
  if (s.memoryBytes !== undefined) info.memoryBytes = s.memoryBytes;
  if (s.exit !== undefined) {
    info.exit = {};
    if (s.exit.code !== undefined) info.exit.code = s.exit.code;
    if (s.exit.signal !== undefined) info.exit.signal = s.exit.signal;
  }
  return info;
}

/** Where sessions live: `$PERCH_RUNTIME_DIR`, else `/tmp/perch-<uid>`, as the binary decides. */
export function runtimeDir(env: Readonly<Record<string, string | undefined>>, uid: number, cwd: string): string {
  const configured = env.PERCH_RUNTIME_DIR;
  if (configured !== undefined && configured !== '') return path.resolve(cwd, configured);
  return `/tmp/perch-${uid}`;
}

/** What a terminal's launch said about a session not listed yet. */
export interface Launched {
  name?: string;
  kind?: string;
  owner?: string;
  annotations?: Readonly<Record<string, string>>;
  cwd?: string;
}

/**
 * A session a terminal of this window is creating, before a listing shows
 * it: `starting`, with what the launch said. The directory is known from the
 * ID, so a caller can find `{dir}` at once.
 */
export function startingInfo(id: string, dir: string, launched: Launched = {}): SessionInfo {
  const info: SessionInfo = {
    id,
    name: launched.name ?? '',
    kind: launched.kind ?? '',
    annotations: { ...launched.annotations },
    dir: path.join(dir, id),
    degraded: [],
    cwd: launched.cwd ?? '',
    host: '',
    createdAt: '',
    state: 'starting',
    attached: false,
    attachedHere: false,
  };
  if (launched.owner !== undefined) info.owner = launched.owner;
  return info;
}

export function matchesFilter(s: SessionInfo, filter: { owner?: string; kind?: string } | undefined): boolean {
  if (filter?.owner !== undefined && s.owner !== filter.owner) return false;
  if (filter?.kind !== undefined && s.kind !== filter.kind) return false;
  return true;
}

/**
 * Why a call failed. `send` always has one:
 * - `queue_full`: nothing was queued; the queue had no room (or the text is
 *   over the 1 MiB it holds).
 * - `outcome_unknown`: the request may have been queued. Never resend.
 * - `no_session`: no such session, or it has ended.
 * - `unavailable`: Perch cannot run here, or cannot reach the session.
 */
export type ApiErrorCode = 'queue_full' | 'outcome_unknown' | 'no_session' | 'unavailable';

export class PerchApiError extends Error {
  constructor(
    readonly code: ApiErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'PerchApiError';
  }
}

/** The code of a failed `perch send`. */
export function sendErrorCode(err: unknown): ApiErrorCode {
  if (!(err instanceof PerchError)) return 'unavailable';
  if (err.spawnError !== undefined) return 'unavailable';
  if (err.exitCode === 3) return 'queue_full';
  // A send cut off by the time limit may have reached the session.
  if (err.exitCode === 4 || err.timedOut) return 'outcome_unknown';
  if (endedOrMissing(err.stderr)) return 'no_session';
  if (/over the 1 MiB limit/.test(err.stderr)) return 'queue_full';
  return 'unavailable';
}

/** The code of another failed call, if one fits. */
export function errorCode(err: unknown): ApiErrorCode | undefined {
  if (!(err instanceof PerchError)) return undefined;
  if (err.spawnError !== undefined) return 'unavailable';
  if (endedOrMissing(err.stderr)) return 'no_session';
  return undefined;
}

function endedOrMissing(stderr: string): boolean {
  // `kill` says "has already ended"; `send` and `attach` say "has ended".
  return /^perch: no session /m.test(stderr) || /^perch: session \S+ has (already )?ended/m.test(stderr);
}

/** An Error for the API: a PerchApiError when a code fits, else the error itself. */
export function apiError(err: unknown, code: ApiErrorCode | undefined): Error {
  const message = err instanceof Error ? err.message : String(err);
  if (code !== undefined) return new PerchApiError(code, message);
  return err instanceof Error ? err : new Error(message);
}

export interface EndReporterDeps {
  /** `perch info`: the session now, undefined if gone. Throws when it cannot tell. */
  info(id: string): Promise<Session | undefined>;
  /** The latest listing's view of a session. */
  find(id: string): Session | undefined;
  /** The clock listings are timed by (their `startedAt`). */
  now(): number;
}

/**
 * Reports each session's end once. A listing shows a session ending as a
 * tombstone with its exit status, or as a session gone without one. A
 * session that exits while attached leaves no tombstone, so its exit is
 * known only to the window whose terminal received it, and one that
 * starts and ends between two listings is known only from its attach.
 */
export class EndReporter {
  private readonly reported = new Set<string>();
  private readonly exits = new Map<string, SessionExit>();
  /** Unconfirmed ends: the session as last seen, and when it was left pending. */
  private readonly pending = new Map<string, { known: Session | undefined; since: number }>();
  /** Sessions this window's terminals attached to, as last seen. */
  private readonly seen = new Map<string, Session>();

  constructor(private readonly deps: EndReporterDeps) {}

  /** A terminal of this window attached to the session. */
  attached(s: Session): void {
    this.seen.set(s.id, s);
  }

  /**
   * A terminal of this window closed. If it ended with its process (not a
   * busy attach) and `perch info` confirms the program ended (a tombstone,
   * or no session at all; a detach leaves it running), resolves to the
   * session to report, at most once, rather than wait for a listing. If
   * `perch info` fails, the next successful listing decides.
   */
  async closed(o: CloseOutcome): Promise<Session | undefined> {
    if (o.reason !== 'process' || o.result === 'busy') return undefined;
    if (o.code !== undefined) this.exits.set(o.id, { code: o.code });
    const known = (): Session | undefined => this.deps.find(o.id) ?? this.seen.get(o.id);
    let now: Session | undefined;
    try {
      now = await this.deps.info(o.id);
    } catch {
      this.pending.set(o.id, { known: known(), since: this.deps.now() });
      this.seen.delete(o.id);
      return undefined;
    }
    if (now !== undefined && !isEnded(now)) return undefined;
    // `now` is the tombstone, or undefined if the session left none (it
    // exited while attached).
    const s = now ?? withUnknownEnd(known());
    this.seen.delete(o.id);
    return s === undefined ? undefined : this.ended([s])[0];
  }

  /**
   * At each successful listing: a pending session that is absent, or a
   * tombstone, ended, and is reported once (exit status unknown unless this
   * window's terminal received it). One the listing shows live, as after a
   * takeover, is dropped unreported; an unresponsive one stays pending.
   * Only a listing started after the session was left pending decides: one
   * already under way may predate the session, or a takeover of it.
   */
  reconcile(listed: readonly Session[], startedAt: number = Infinity): Session[] {
    const out: Session[] = [];
    for (const [id, { known, since }] of [...this.pending]) {
      if (startedAt <= since) continue;
      const s = listed.find((x) => x.id === id);
      if (s !== undefined && !isEnded(s)) {
        if (s.state !== 'unresponsive') this.pending.delete(id);
        continue;
      }
      this.pending.delete(id);
      const ended = s ?? withUnknownEnd(known);
      if (ended !== undefined) out.push(...this.ended([ended]));
    }
    return out;
  }

  /** From a listing's `ended`: the sessions to report, each once. */
  ended(sessions: readonly Session[]): Session[] {
    const out: Session[] = [];
    for (const s of sessions) {
      if (!isEnded(s) || this.reported.has(s.id)) continue;
      this.reported.add(s.id);
      const exit = s.exit ?? this.exits.get(s.id);
      out.push(exit === s.exit ? s : { ...s, exit });
      this.exits.delete(s.id);
    }
    return out;
  }
}

/** A session last seen live, as ended with its exit status unknown. */
function withUnknownEnd(known: Session | undefined): Session | undefined {
  return known && { ...known, state: 'exited', attached: false, exit: undefined };
}
