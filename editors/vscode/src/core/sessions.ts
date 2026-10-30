// SPDX-License-Identifier: GPL-3.0-only

// Sessions as `perch list --json --all` reports them, how the list and the
// kill confirmation describe them, and what changed between two listings.

export type SessionState = 'starting' | 'running' | 'exited' | 'lost' | 'unresponsive';

export interface SessionExit {
  code?: number;
  signal?: string;
  signalNumber?: number;
  error?: string;
}

export interface Session {
  id: string;
  name: string;
  kind: string;
  owner?: string;
  annotations: Record<string, string>;
  /** The session directory. */
  dir: string;
  degraded: string[];
  cwd: string;
  host: string;
  createdAt: string;
  state: SessionState;
  attached: boolean;
  clientLabel?: string;
  lastOutputAt?: string;
  memoryBytes?: number;
  exit?: SessionExit;
}

const states: ReadonlySet<string> = new Set(['starting', 'running', 'exited', 'lost', 'unresponsive']);

/** Parses `perch list --json`. Throws on anything but schema 1. */
export function parseList(stdout: string): Session[] {
  const doc: unknown = JSON.parse(stdout);
  if (!isObject(doc) || doc.schema !== 1 || !Array.isArray(doc.sessions)) {
    throw new Error('perch list --json: unexpected output');
  }
  return doc.sessions.filter(isObject).map(parseSession).filter((s): s is Session => s !== undefined);
}

/** Parses `perch info --json`: one session. */
export function parseInfo(stdout: string): Session {
  const doc: unknown = JSON.parse(stdout);
  const s = isObject(doc) ? parseSession(doc) : undefined;
  if (s === undefined) throw new Error('perch info --json: unexpected output');
  return s;
}

function parseSession(v: Record<string, unknown>): Session | undefined {
  const id = str(v.id);
  const state = str(v.state);
  if (id === '' || !states.has(state)) return undefined;
  const annotations: Record<string, string> = {};
  if (isObject(v.annotations)) {
    for (const [k, x] of Object.entries(v.annotations)) if (typeof x === 'string') annotations[k] = x;
  }
  const s: Session = {
    id,
    name: str(v.name),
    kind: str(v.kind),
    annotations,
    dir: str(v.dir),
    degraded: Array.isArray(v.degraded) ? v.degraded.filter((x): x is string => typeof x === 'string') : [],
    cwd: str(v.cwd),
    host: str(v.host),
    createdAt: str(v.createdAt),
    state: state as SessionState,
    attached: v.attached === true,
  };
  if (typeof v.owner === 'string') s.owner = v.owner;
  if (typeof v.clientLabel === 'string') s.clientLabel = v.clientLabel;
  if (typeof v.lastOutputAt === 'string') s.lastOutputAt = v.lastOutputAt;
  if (typeof v.memoryBytes === 'number') s.memoryBytes = v.memoryBytes;
  if (isObject(v.exit)) {
    const e = v.exit;
    s.exit = {};
    if (typeof e.code === 'number') s.exit.code = e.code;
    if (typeof e.signal === 'string') s.exit.signal = e.signal;
    if (typeof e.signalNumber === 'number') s.exit.signalNumber = e.signalNumber;
    if (typeof e.error === 'string') s.exit.error = e.error;
  }
  return s;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

/** Exited or lost: a tombstone, shown with "Show last output" and "Dismiss". */
export function isEnded(s: Session): boolean {
  return s.state === 'exited' || s.state === 'lost';
}

/** Running with no client: what the status bar counts. */
export function isDetached(s: Session): boolean {
  return s.state === 'running' && !s.attached;
}

export function countDetached(sessions: readonly Session[]): number {
  return sessions.filter(isDetached).length;
}

/** The name, or the ID for a session without one. */
export function displayName(s: Session): string {
  return s.name !== '' ? s.name : s.id;
}

const kindNames: Record<string, string> = { r: 'R', stata: 'Stata', shell: 'Shell' };

/** `R`, `Stata`, `Shell`, or the kind as given; `Program` if empty. */
export function kindName(kind: string): string {
  return kindNames[kind] ?? (kind !== '' ? kind : 'Program');
}

/** `exit code 0`, `killed by signal 9 (SIGKILL)`, as the CLI words it. */
export function exitText(e: SessionExit | undefined): string {
  if (e === undefined) return 'exit status unknown';
  if (e.error !== undefined) return e.error;
  if (e.code !== undefined) return `exit code ${e.code}`;
  if (e.signalNumber !== undefined && e.signal !== undefined) return `killed by signal ${e.signalNumber} (${e.signal})`;
  if (e.signal !== undefined) return `killed by ${e.signal}`;
  if (e.signalNumber !== undefined) return `killed by signal ${e.signalNumber}`;
  return 'exit status unknown';
}

/** `5s`, `3m`, `2h`, `4d`, as `perch list` shows ages. */
export function shortAge(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/** The age of a session at `now` (milliseconds), or `?`. */
export function sessionAge(s: Session, now: number): string {
  const t = Date.parse(s.createdAt);
  return Number.isNaN(t) ? '?' : shortAge((now - t) / 1000);
}

/** `about 4.1 GB`: resident memory is an estimate. */
export function memoryText(bytes: number): string {
  const units = ['bytes', 'KB', 'MB', 'GB', 'TB'];
  let v = bytes;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  const n = i === 0 ? String(Math.round(v)) : v >= 100 ? v.toFixed(0) : v.toFixed(1);
  return `about ${n} ${units[i]}`;
}

/** `/home/me/thesis` as `~/thesis`. */
export function homeRelative(p: string, home: string): string {
  if (home === '' || home === '/') return p;
  const h = home.endsWith('/') ? home.slice(0, -1) : home;
  if (p === h) return '~';
  if (p.startsWith(h + '/')) return `~${p.slice(h.length)}`;
  return p;
}

export interface DescribeContext {
  now: number;
  home: string;
  /** This window's client label, to tell "attached here" from "attached". */
  label: string;
}

/** Whether this window's client is the one attached. */
export function attachedHere(s: Session, label: string): boolean {
  return s.attached && s.clientLabel === label;
}

/** The connection or end state, as the list item's last part shows it. */
export function stateText(s: Session, label: string): string {
  switch (s.state) {
    case 'running':
      return attachedHere(s, label) ? 'attached here' : s.attached ? 'attached' : 'detached';
    case 'exited':
      return `exited, ${exitText(s.exit)}`;
    case 'lost':
      return 'lost';
    default:
      return s.state;
  }
}

/** `R · ~/thesis · 3h · about 4.1 GB · detached`. */
export function describe(s: Session, ctx: DescribeContext): string {
  const parts = [kindName(s.kind)];
  if (s.cwd !== '') parts.push(homeRelative(s.cwd, ctx.home));
  parts.push(sessionAge(s, ctx.now));
  if (s.memoryBytes !== undefined) parts.push(memoryText(s.memoryBytes));
  parts.push(stateText(s, ctx.label));
  if (s.degraded.length > 0) parts.push(`warning: ${s.degraded.join(', ')}`);
  return parts.join(' · ');
}

/** The kill confirmation: name, kind, working directory, age and memory. */
export function killConfirmation(s: Session, ctx: DescribeContext): { message: string; detail: string } {
  const lines = [
    `Kind: ${kindName(s.kind)}`,
    `Working directory: ${s.cwd !== '' ? homeRelative(s.cwd, ctx.home) : 'unknown'}`,
    `Age: ${sessionAge(s, ctx.now)}`,
    `Memory: ${s.memoryBytes !== undefined ? memoryText(s.memoryBytes) : 'unknown'}`,
  ];
  if (s.attached) lines.push(attachedHere(s, ctx.label) ? 'Attached in this window.' : 'Attached in another window.');
  return {
    message: `Kill ${displayName(s)}?`,
    detail: `${lines.join('\n')}\n\nThe program and everything it started in its terminal will be ended.`,
  };
}

export interface ListDiff {
  added: Session[];
  /** Present in both, with a different state, attachment or name. */
  changed: Session[];
  /** Gone from the listing. */
  removed: Session[];
  /**
   * Sessions seen to end: running before and now exited or lost, or gone
   * without a tombstone, which leaves the exit status unknown.
   */
  ended: Session[];
}

/** What changed from one listing to the next. */
export function diffSessions(before: readonly Session[], after: readonly Session[]): ListDiff {
  const old = new Map(before.map((s) => [s.id, s]));
  const now = new Map(after.map((s) => [s.id, s]));
  const diff: ListDiff = { added: [], changed: [], removed: [], ended: [] };
  for (const s of after) {
    const prev = old.get(s.id);
    if (prev === undefined) {
      diff.added.push(s);
      continue;
    }
    if (
      prev.state !== s.state ||
      prev.attached !== s.attached ||
      prev.clientLabel !== s.clientLabel ||
      prev.name !== s.name ||
      prev.cwd !== s.cwd ||
      prev.memoryBytes !== s.memoryBytes ||
      prev.degraded.join() !== s.degraded.join()
    ) {
      diff.changed.push(s);
    }
    if (!isEnded(prev) && isEnded(s)) diff.ended.push(s);
  }
  for (const s of before) {
    if (now.has(s.id)) continue;
    diff.removed.push(s);
    if (!isEnded(s)) diff.ended.push({ ...s, state: 'exited', attached: false, exit: undefined });
  }
  return diff;
}

export function isEmptyDiff(d: ListDiff): boolean {
  return d.added.length + d.changed.length + d.removed.length === 0;
}

/** Text with CSI, OSC and other escape sequences removed, for `tail.txt`. */
export function stripAnsi(text: string): string {
  /* eslint-disable no-control-regex -- escape sequences are the point */
  return text
    .replace(/\x1b\][\s\S]*?(?:\x07|\x1b\\)/g, '')
    .replace(/\x1b[P_^X][\s\S]*?\x1b\\/g, '')
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
    .replace(/\x1b[ -/]*[0-~]/g, '')
    .replace(/\r\n/g, '\n');
  /* eslint-enable no-control-regex */
}

/** The codicon for a list item: the kind while live, the state once ended. */
export function iconName(s: Session): string {
  if (s.state === 'lost' || s.state === 'unresponsive' || s.degraded.length > 0) return 'warning';
  if (s.state === 'exited') return 'circle-slash';
  return kindIcon(s.kind);
}

/** The codicon for a kind of session. */
export function kindIcon(kind: string): string {
  switch (kind) {
    case 'r':
      return 'graph';
    case 'stata':
      return 'table';
    case 'shell':
      return 'terminal';
    default:
      return 'window';
  }
}
