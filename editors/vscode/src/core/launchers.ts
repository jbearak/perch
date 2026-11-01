// SPDX-License-Identifier: GPL-3.0-only

// Consoles other extensions declare for "New Persistent Terminal...", in
// their package.json:
//
//   "perch": { "launchers": [{ "label": "...", "command": "...", "description": "...", "icon": "..." }] }
//
// Picking one runs the command, which starts the console in a Perch session.
import { codicon } from './profiles';

export interface Launcher {
  label: string;
  description: string;
  /** A codicon ID. */
  icon: string;
  command: string;
  extensionId: string;
}

/** What Perch reads of an installed extension. */
export interface InstalledExtension {
  id: string;
  packageJSON: unknown;
}

const nonEmpty = (v: unknown): v is string => typeof v === 'string' && v.trim() !== '';

/** The launchers the extensions declare, in extension then declaration order. */
export function declaredLaunchers(extensions: readonly InstalledExtension[], log: (message: string) => void): Launcher[] {
  const out: Launcher[] = [];
  for (const ext of extensions) {
    const manifest = ext.packageJSON as { perch?: { launchers?: unknown }; displayName?: unknown } | undefined;
    const declared = manifest?.perch?.launchers;
    if (declared === undefined) continue;
    if (!Array.isArray(declared)) {
      log(`${ext.id}: perch.launchers is not a list; ignored`);
      continue;
    }
    const displayName = manifest?.displayName;
    const fallback = nonEmpty(displayName) && !displayName.startsWith('%') ? displayName : ext.id;
    for (const entry of declared) {
      const e = entry as { label?: unknown; command?: unknown; description?: unknown; icon?: unknown } | null;
      if (e === null || typeof e !== 'object' || !nonEmpty(e.label) || !nonEmpty(e.command)) {
        log(`${ext.id}: a perch.launchers entry needs a label and a command; ignored`);
        continue;
      }
      out.push({
        label: e.label,
        description: nonEmpty(e.description) ? e.description : fallback,
        icon: typeof e.icon === 'string' && codicon.test(e.icon) ? e.icon : 'terminal',
        command: e.command,
        extensionId: ext.id,
      });
    }
  }
  return out;
}
