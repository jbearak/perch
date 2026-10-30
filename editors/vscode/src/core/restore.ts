// SPDX-License-Identifier: GPL-3.0-only

// Restoring terminals. Perch terminals are transient, so VS Code does not
// revive them; instead a workspace keeps the IDs of the sessions attached
// there, and the next activation reopens the ones that are detached. These
// are the decisions, without VS Code: what a terminal's close means for the
// restore set, what to do with each session in it, and the 60-second window
// in which pending restores are retried.
import { workspaceKeyOf } from './labels';
import { Recognized } from './launch';
import { isEnded, Session } from './sessions';

/** How long after activation pending restores are retried. */
export const RESTORE_WINDOW_MS = 60_000;

/** The exit status code a busy `perch attach --if-detached` exits with. */
export const EXIT_BUSY = 75;

/** `vscode.TerminalExitReason`, by name. */
export type ExitReason = 'unknown' | 'shutdown' | 'process' | 'user' | 'extension';

/**
 * What a closed Perch terminal means for its session's place in the
 * restore set. `User`, and `Process` with any code but 75, remove it.
 * `Shutdown` keeps it. Code 75 may be a busy attach or a program that
 * exited with 75, so `perch info` decides. A terminal the extension closed
 * itself was already decided.
 */
export function closeAction(reason: ExitReason, code: number | undefined): 'forget' | 'keep' | 'confirm' | 'ignore' {
  switch (reason) {
    case 'user':
      return 'forget';
    case 'process':
      return code === EXIT_BUSY ? 'confirm' : 'forget';
    case 'extension':
      return 'ignore';
    default:
      return 'keep';
  }
}

/**
 * After an exit with 75: `busy` if the session is still running and attached
 * elsewhere, so the attach was refused; `ended` if the program itself exited
 * with 75 (or the session is gone).
 */
export function after75(info: Session | undefined): 'busy' | 'ended' {
  return info !== undefined && info.state === 'running' && info.attached ? 'busy' : 'ended';
}

/**
 * The next step for a session in the restore set, from `perch info`:
 * - `forget`: it has ended or is gone.
 * - `open`: it is detached; open a terminal with `--if-detached`.
 * - `done`: this window's client is attached already.
 * - `retry`: not ready yet, or attached by a client with this workspace's key,
 *   which may be the previous incarnation of this window still exiting.
 * - `leave`: attached from another workspace. Restore never takes a session
 *   from a live client.
 */
export function restoreStep(
  info: Session | undefined,
  label: string,
): 'forget' | 'open' | 'done' | 'retry' | 'leave' {
  if (info === undefined || isEnded(info)) return 'forget';
  if (info.state !== 'running') return 'retry';
  if (!info.attached) return 'open';
  if (info.clientLabel === label) return 'done';
  const here = workspaceKeyOf(label);
  return here !== undefined && workspaceKeyOf(info.clientLabel) === here ? 'retry' : 'leave';
}

/** Where the restore set lives: `workspaceState` in VS Code. */
export interface RestoreStore {
  get(): readonly string[];
  set(ids: readonly string[]): PromiseLike<void>;
}

/** The IDs of sessions attached in this workspace, in order of attaching. */
export class RestoreSet {
  constructor(private readonly store: RestoreStore) {}

  ids(): string[] {
    return this.store.get().filter((x) => typeof x === 'string');
  }

  has(id: string): boolean {
    return this.ids().includes(id);
  }

  async add(id: string): Promise<void> {
    const ids = this.ids();
    if (!ids.includes(id)) await this.store.set([...ids, id]);
  }

  async remove(id: string): Promise<void> {
    const ids = this.ids();
    if (ids.includes(id)) await this.store.set(ids.filter((x) => x !== id));
  }
}

export interface RestorerDeps {
  set: RestoreSet;
  /** This window's client label. */
  label: string;
  /** `perch info <id>`, or undefined if there is no such session. */
  info(id: string): Promise<Session | undefined>;
  /** Whether a terminal in this window is already attached to the session. */
  hasTerminal(id: string): boolean;
  /** Opens a terminal running `perch attach <id> --if-detached --restore full`. */
  open(session: Session): void;
  now(): number;
  /** Reports a failure to check a session; the restore goes on. */
  log(message: string): void;
}

/**
 * Restores a workspace's terminals after activation, adopted ones aside,
 * and retries the pending ones on each list refresh for 60 seconds.
 */
export class Restorer {
  private readonly pending = new Set<string>();
  private startedAt: number | undefined;
  private running: Promise<void> | undefined;
  private again = false;

  constructor(private readonly deps: RestorerDeps) {}

  /** The first pass, after adoption. */
  start(): Promise<void> {
    this.startedAt = this.deps.now();
    for (const id of this.deps.set.ids()) {
      if (!this.deps.hasTerminal(id)) this.pending.add(id);
    }
    return this.pass();
  }

  /** IDs still waiting for a retry. */
  pendingIds(): string[] {
    return [...this.pending];
  }

  /** Whether the 60-second window is still open. */
  retrying(): boolean {
    return this.startedAt !== undefined && this.deps.now() - this.startedAt <= RESTORE_WINDOW_MS;
  }

  /** A busy attach: the session stays in the set and is retried while the window is open. */
  busy(id: string): void {
    if (this.retrying()) this.pending.add(id);
  }

  /** On each list refresh. One pass runs at a time; calls during one coalesce. */
  pass(): Promise<void> {
    if (this.running !== undefined) {
      this.again = true;
      return this.running;
    }
    this.running = (async () => {
      try {
        do {
          this.again = false;
          await this.once();
        } while (this.again);
      } finally {
        this.running = undefined;
      }
    })();
    return this.running;
  }

  private async once(): Promise<void> {
    if (!this.retrying()) {
      this.pending.clear();
      return;
    }
    for (const id of [...this.pending]) {
      if (this.deps.hasTerminal(id)) {
        this.pending.delete(id);
        continue;
      }
      let info: Session | undefined;
      try {
        info = await this.deps.info(id);
      } catch (err) {
        this.deps.log(`Restore: checking ${id} failed: ${err instanceof Error ? err.message : String(err)}`);
        continue;
      }
      switch (restoreStep(info, this.deps.label)) {
        case 'forget':
          this.pending.delete(id);
          await this.deps.set.remove(id);
          break;
        case 'open':
          this.pending.delete(id);
          if (info !== undefined && !this.deps.hasTerminal(id)) this.deps.open(info);
          break;
        case 'done':
        case 'leave':
          this.pending.delete(id);
          break;
        case 'retry':
          break;
      }
    }
  }
}

/** A terminal Perch recognizes, and the session it belongs to. */
export interface Tracked<T> {
  terminal: T;
  launch: Recognized;
}

/**
 * The Perch terminals open in this window. Adding is idempotent, so adoption
 * at activation and `onDidOpenTerminal` can both see the same terminal in
 * any order.
 */
export class TerminalRegistry<T> {
  private readonly byTerminal = new Map<T, Recognized>();

  /** Returns true if the terminal was not tracked before. */
  track(terminal: T, launch: Recognized): boolean {
    if (this.byTerminal.has(terminal)) return false;
    this.byTerminal.set(terminal, launch);
    return true;
  }

  untrack(terminal: T): Recognized | undefined {
    const launch = this.byTerminal.get(terminal);
    this.byTerminal.delete(terminal);
    return launch;
  }

  get(terminal: T): Recognized | undefined {
    return this.byTerminal.get(terminal);
  }

  /** The newest terminal attached to the session. */
  terminalFor(id: string): T | undefined {
    let found: T | undefined;
    for (const [t, l] of this.byTerminal) if (l.id === id) found = t;
    return found;
  }

  all(): Tracked<T>[] {
    return [...this.byTerminal].map(([terminal, launch]) => ({ terminal, launch }));
  }
}

/**
 * Adoption at activation: every terminal recognized from its creation
 * options is tracked as it is. Returns the ones newly tracked.
 */
export function adopt<T>(
  registry: TerminalRegistry<T>,
  terminals: readonly T[],
  recognizeTerminal: (t: T) => Recognized | undefined,
): Tracked<T>[] {
  const adopted: Tracked<T>[] = [];
  for (const terminal of terminals) {
    const launch = recognizeTerminal(terminal);
    if (launch !== undefined && registry.track(terminal, launch)) adopted.push({ terminal, launch });
  }
  return adopted;
}
