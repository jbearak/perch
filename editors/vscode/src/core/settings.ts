// SPDX-License-Identifier: GPL-3.0-only

// The extension's settings, read through a getter so tests need no VS Code.

export interface Settings {
  /** `perch.path`: a perch binary to run instead of the bundled one. */
  path: string;
  /** `perch.addToPath`: put the stable copy on new terminals' `PATH`. */
  addToPath: boolean;
  /** `perch.restoreTerminals`: reopen this workspace's sessions on activation. */
  restoreTerminals: boolean;
  /** `perch.linkedEnvironment`: names passed to `perch new --link-env`. */
  linkedEnvironment: string[];
  /** `perch.shell`: the persistent shell's command; empty means `$SHELL`. */
  shell: string | string[];
}

export const defaults: Settings = {
  path: '',
  addToPath: true,
  restoreTerminals: true,
  linkedEnvironment: [],
  shell: '',
};

/** Reads `perch.*`, falling back to the default for a missing or mistyped value. */
export function readSettings(get: (key: keyof Settings) => unknown): Settings {
  const path = get('path');
  const addToPath = get('addToPath');
  const restoreTerminals = get('restoreTerminals');
  const linked = get('linkedEnvironment');
  const shell = get('shell');
  return {
    path: typeof path === 'string' ? path.trim() : defaults.path,
    addToPath: typeof addToPath === 'boolean' ? addToPath : defaults.addToPath,
    restoreTerminals: typeof restoreTerminals === 'boolean' ? restoreTerminals : defaults.restoreTerminals,
    linkedEnvironment: Array.isArray(linked)
      ? [...new Set(linked.filter((x): x is string => typeof x === 'string' && /^[A-Za-z_][A-Za-z0-9_]*$/.test(x)))]
      : defaults.linkedEnvironment,
    shell:
      typeof shell === 'string'
        ? shell
        : Array.isArray(shell) && shell.every((x) => typeof x === 'string')
          ? (shell as string[])
          : defaults.shell,
  };
}
