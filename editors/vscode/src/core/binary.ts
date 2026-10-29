// SPDX-License-Identifier: GPL-3.0-only

// Which perch binary the extension runs, and where it keeps the stable copy
// for terminals. Plain logic: no `vscode` imports in src/core/.
import * as fs from 'fs';
import * as path from 'path';

/** Perch runs on macOS and Linux hosts only. */
export function hostSupported(platform: NodeJS.Platform): boolean {
  return platform === 'darwin' || platform === 'linux';
}

/** `<os>-<arch>` as Node names them, such as `linux-x64`. */
export function platformDir(platform: NodeJS.Platform, arch: string): string {
  return `${platform}-${arch}`;
}

/** The bundled binary for a platform and architecture. */
export function bundledBinaryPath(
  extensionPath: string,
  platform: NodeJS.Platform,
  arch: string,
): string {
  return path.join(extensionPath, 'bin', platformDir(platform, arch), 'perch');
}

/**
 * Where `perch self-install` keeps the stable copy: global storage survives
 * updates, and the `<os>-<arch>` part keeps a home directory shared between
 * x64 and arm64 hosts working.
 */
export function commandLineDir(
  globalStoragePath: string,
  platform: NodeJS.Platform,
  arch: string,
): string {
  return path.join(globalStoragePath, 'bin', platformDir(platform, arch));
}

export interface BinaryChoice {
  path: string;
  /** `setting` for `perch.path`, else `bundled`. */
  source: 'setting' | 'bundled';
}

/**
 * The binary the extension runs: `perch.path` if set, else the bundled one.
 * A leading `~/` in the setting means the home directory.
 */
export function resolveBinary(options: {
  configuredPath: string;
  extensionPath: string;
  platform: NodeJS.Platform;
  arch: string;
  home: string;
}): BinaryChoice {
  const configured = options.configuredPath.trim();
  if (configured !== '') {
    const expanded =
      configured === '~' || configured.startsWith('~/')
        ? path.join(options.home, configured.slice(1))
        : configured;
    return { path: expanded, source: 'setting' };
  }
  return {
    path: bundledBinaryPath(options.extensionPath, options.platform, options.arch),
    source: 'bundled',
  };
}

/** The `PATH` suffix for new terminals: appended, so the user's own perch wins. */
export function pathAppendValue(dir: string, delimiter: string = path.delimiter): string {
  return `${delimiter}${dir}`;
}

/**
 * Makes the bundled binary executable (0755) in case a registry's archive
 * dropped the bit. Returns whether it had to.
 */
export function ensureExecutable(file: string): boolean {
  const mode = fs.statSync(file).mode & 0o777;
  if ((mode & 0o111) === 0o111) return false;
  fs.chmodSync(file, 0o755);
  return true;
}

/** Perch is available on a macOS or Linux host whose binary runs. */
export async function available(platform: NodeJS.Platform, binaryWorks: () => Promise<boolean>): Promise<boolean> {
  return hostSupported(platform) && (await binaryWorks());
}
