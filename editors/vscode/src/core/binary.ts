// SPDX-License-Identifier: GPL-3.0-only

// Where the extension keeps the perch binary it ships. Plain logic: no
// `vscode` imports in src/core/.
import * as path from 'path';

/** The bundled binary for a platform and architecture, as Node names them. */
export function bundledBinaryPath(
  extensionPath: string,
  platform: NodeJS.Platform,
  arch: string,
): string {
  return path.join(extensionPath, 'bin', `${platform}-${arch}`, 'perch');
}
