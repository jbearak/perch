// SPDX-License-Identifier: GPL-3.0-only

// The extension's settings, read through a getter so tests need no VS Code.

export interface Settings {
  /** `perch.path`: a perch binary to run instead of the bundled one. */
  path: string;
  /** `perch.addToPath`: put the stable copy on new terminals' `PATH`. */
  addToPath: boolean;
}

export const defaults: Settings = { path: '', addToPath: true };

/** Reads `perch.*`, falling back to the default for a missing or mistyped value. */
export function readSettings(get: (key: keyof Settings) => unknown): Settings {
  const path = get('path');
  const addToPath = get('addToPath');
  return {
    path: typeof path === 'string' ? path.trim() : defaults.path,
    addToPath: typeof addToPath === 'boolean' ? addToPath : defaults.addToPath,
  };
}
