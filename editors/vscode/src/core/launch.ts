// SPDX-License-Identifier: GPL-3.0-only

// The command lines of Perch terminals, and recognizing a Perch terminal
// from the shell path and arguments it was created with.
import { randomBytes } from 'crypto';
import * as path from 'path';

/** The most history lines a session keeps; `perch new` clamps to it too. */
export const MAX_SCROLLBACK = 10_000;
export const DEFAULT_SCROLLBACK = 1000;

const idAlphabet = 'abcdefghijklmnopqrstuvwxyz0123456789';

/** A session ID as `perch new --id` takes it: 8 characters from [a-z0-9]. */
export function newSessionId(random: (n: number) => Buffer = randomBytes): string {
  // 252 is the largest multiple of 36 below 256: rejecting bytes above it
  // keeps every character equally likely.
  let id = '';
  while (id.length < 8) {
    for (const b of random(16)) {
      if (b < 252 && id.length < 8) id += idAlphabet[b % 36];
    }
  }
  return id;
}

export function isSessionId(s: string): boolean {
  return /^[a-z0-9]{8}$/.test(s);
}

/** `terminal.integrated.scrollback` as `--scrollback` takes it. */
export function scrollbackLines(setting: unknown): number {
  if (typeof setting !== 'number' || !Number.isFinite(setting) || setting < 0) return DEFAULT_SCROLLBACK;
  return Math.min(Math.floor(setting), MAX_SCROLLBACK);
}

export interface NewSession {
  id: string;
  label: string;
  scrollback: number;
  name: string;
  kind: string;
  command: readonly string[];
  owner?: string;
  cwd?: string;
  env?: Readonly<Record<string, string>>;
  annotations?: Readonly<Record<string, string>>;
  /** `--link-env` names; empty when the binary does not support it. */
  linkEnv?: readonly string[];
}

/**
 * `perch new` for a terminal: it attaches at once, with no detach key
 * (closing the terminal detaches) and full restore when it re-attaches.
 */
export function newArgs(o: NewSession): string[] {
  const args = ['new', '--id', o.id, '--label', o.label, '--scrollback', String(o.scrollback)];
  args.push('--detach-key', 'none', '--restore', 'full');
  if (o.name !== '') args.push('--name', o.name);
  if (o.kind !== '') args.push('--kind', o.kind);
  if (o.owner !== undefined && o.owner !== '') args.push('--owner', o.owner);
  if (o.cwd !== undefined && o.cwd !== '') args.push('--cwd', o.cwd);
  for (const [k, v] of Object.entries(o.annotations ?? {})) args.push('--annotate', `${k}=${v}`);
  for (const [k, v] of Object.entries(o.env ?? {})) args.push('--env', `${k}=${v}`);
  for (const name of o.linkEnv ?? []) args.push('--link-env', name);
  args.push('--', ...o.command);
  return args;
}

/**
 * `perch attach` for a terminal. `ifDetached` is restore's guard against
 * another window getting there first: the attach then exits with 75.
 */
export function attachArgs(id: string, label: string, scrollback: number, ifDetached = false): string[] {
  const args = ['attach', id, '--label', label, '--restore', 'full', '--detach-key', 'none'];
  args.push('--scrollback', String(scrollback));
  if (ifDetached) args.push('--if-detached');
  return args;
}

export interface Recognized {
  id: string;
  /** `new` for a terminal that created its session, `attach` for one that attached. */
  mode: 'new' | 'attach';
  ifDetached: boolean;
  /** The client label it attaches with, if it gives one. */
  label?: string;
}

/**
 * A Perch terminal from its creation options: a shell path whose file name
 * is `perch` (the full path may point at an extension directory an update
 * has since deleted), and arguments `new --id <id> ...` or `attach <id> ...`.
 */
export function recognize(shellPath: unknown, shellArgs: unknown): Recognized | undefined {
  if (typeof shellPath !== 'string' || path.posix.basename(shellPath) !== 'perch') return undefined;
  if (!Array.isArray(shellArgs) || !shellArgs.every((a) => typeof a === 'string')) return undefined;
  const args = shellArgs as string[];
  const end = args.indexOf('--');
  const options = end === -1 ? args : args.slice(0, end);
  let r: Recognized | undefined;
  if (args[0] === 'new' && args[1] === '--id' && isSessionId(args[2] ?? '')) {
    r = { id: args[2], mode: 'new', ifDetached: false };
  } else if (args[0] === 'attach' && isSessionId(args[1] ?? '')) {
    r = { id: args[1], mode: 'attach', ifDetached: options.includes('--if-detached') };
  }
  const i = options.indexOf('--label');
  if (r !== undefined && i !== -1 && i + 1 < options.length) r.label = options[i + 1];
  return r;
}

/**
 * The persistent shell: `perch.shell` if set, else `$SHELL`, with `-l` on
 * macOS to match VS Code's default there.
 */
export function shellCommand(setting: unknown, platform: NodeJS.Platform, shellEnv: string | undefined): string[] {
  if (Array.isArray(setting)) {
    const parts = setting.filter((x): x is string => typeof x === 'string');
    if (parts.length > 0 && parts[0].trim() !== '') return parts;
  } else if (typeof setting === 'string' && setting.trim() !== '') {
    return [setting.trim()];
  }
  const shell = shellEnv !== undefined && shellEnv !== '' ? shellEnv : '/bin/sh';
  return platform === 'darwin' ? [shell, '-l'] : [shell];
}
