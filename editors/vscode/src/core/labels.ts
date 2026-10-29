// SPDX-License-Identifier: GPL-3.0-only

// Client labels: `<workspace-key>#<window-key>`. The workspace key lets a
// window recognize sessions attached from its own workspace; the window key
// tells this activation from another window on the same workspace.
import { createHash, randomBytes } from 'crypto';

/** The first 16 hex digits of the SHA-256 of the workspace URI, or `none`. */
export function workspaceKey(workspaceUri: string | undefined): string {
  if (workspaceUri === undefined || workspaceUri === '') return 'none';
  return createHash('sha256').update(workspaceUri, 'utf8').digest('hex').slice(0, 16);
}

/** 8 random hex digits, new for each activation. */
export function windowKey(random: (n: number) => Buffer = randomBytes): string {
  return random(4).toString('hex');
}

export function clientLabel(workspace: string, window: string): string {
  return `${workspace}#${window}`;
}

/** The workspace key of a label, or undefined if it is not one of ours. */
export function workspaceKeyOf(label: string | undefined): string | undefined {
  const m = /^(none|[0-9a-f]{16})#[0-9a-f]{8}$/.exec(label ?? '');
  return m ? m[1] : undefined;
}
