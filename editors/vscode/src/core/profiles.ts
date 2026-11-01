// SPDX-License-Identifier: GPL-3.0-only

// "New Persistent Terminal...": the user's VS Code terminal profiles as
// commands `perch new` can run, and a command line typed by the user.
import * as path from 'path';

/** What a profile needs from the host, so tests need no file system. */
export interface ProfileHost {
  env: Readonly<Record<string, string | undefined>>;
  home: string;
  /** The first workspace folder's path, if it is on this host. */
  workspaceFolder?: string;
  isExecutable(file: string): boolean;
}

/** A terminal Perch can start: its command and environment are ready for `perch new`. */
export interface Launchable {
  name: string;
  kind: string;
  command: string[];
  env?: Record<string, string>;
  /** A codicon ID. */
  icon?: string;
  /** A `terminal.*` theme color ID. */
  color?: string;
  /** The command line as the user would read it, for the picker. */
  display: string;
}

/** The `terminal.integrated` keys for this platform, or undefined where Perch does not run. */
export function profileKeys(platform: NodeJS.Platform): { profiles: string; defaultProfile: string } | undefined {
  const os = platform === 'darwin' ? 'osx' : platform === 'linux' ? 'linux' : undefined;
  return os === undefined ? undefined : { profiles: `profiles.${os}`, defaultProfile: `defaultProfile.${os}` };
}

/**
 * `perch new` expands `{dir}` and `{id}` in the command and in `--env`
 * values and rejects any other brace; `{{` and `}}` are literal.
 */
export function literal(s: string): string {
  return s.replace(/[{}]/g, (c) => c + c);
}

/** VS Code variables whose value depends on the editor's state, which Perch does not have. */
const editorVariable =
  /^(command|config|input):|^workspaceFolder|^(workspaceRoot|file|fileBasename|fileBasenameNoExtension|fileDirname|fileDirnameBasename|fileExtname|fileWorkspaceFolder|relativeFile|relativeFileDirname|lineNumber|columnNumber|selectedText|execPath|cwd|defaultBuildTask|extensionInstallFolder)$/;

/**
 * Resolves the variables VS Code allows in a profile that mean the same
 * outside it. Undefined if the text uses any other VS Code variable, or
 * names the workspace folder when there is none. Anything else in `${...}`
 * stays as it is, as VS Code leaves it, such as a shell's `${HOME}`.
 */
export function resolveVariables(s: string, host: ProfileHost): string | undefined {
  let ok = true;
  const out = s.replace(/\$\{([^}]*)\}/g, (text, name: string) => {
    if (name.startsWith('env:')) return host.env[name.slice(4)] ?? '';
    if (name === 'userHome') return host.home;
    if (name === 'pathSeparator' || name === '/') return '/';
    if (name === 'workspaceFolder' && host.workspaceFolder !== undefined) return host.workspaceFolder;
    if (name === 'workspaceFolderBasename' && host.workspaceFolder !== undefined) {
      return path.posix.basename(host.workspaceFolder);
    }
    if (editorVariable.test(name)) ok = false;
    return text;
  });
  return ok ? out : undefined;
}

/** An absolute path that is executable, or a bare name found on `PATH`. */
export function resolveExecutable(file: string, host: ProfileHost): string | undefined {
  if (file === '') return undefined;
  if (path.posix.isAbsolute(file)) return host.isExecutable(file) ? file : undefined;
  if (file.includes('/')) return undefined;
  for (const dir of (host.env.PATH ?? '').split(':')) {
    if (!path.posix.isAbsolute(dir)) continue;
    const candidate = path.posix.join(dir, file);
    if (host.isExecutable(candidate)) return candidate;
  }
  return undefined;
}

const envName = /^[A-Za-z_][A-Za-z0-9_]*$/;
const codicon = /^[a-z0-9]+(-[a-z0-9]+)*$/;

/**
 * One `terminal.integrated.profiles.<os>` entry as a launch, or undefined
 * for one Perch cannot run: removed (`null`), without a path, with a path
 * that does not resolve, or with a variable Perch cannot resolve.
 */
export function profileLaunch(name: string, value: unknown, host: ProfileHost): Launchable | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const p = value as Record<string, unknown>;
  const candidates = typeof p.path === 'string' ? [p.path] : Array.isArray(p.path) ? p.path : [];
  let file: string | undefined;
  for (const c of candidates) {
    if (typeof c !== 'string') continue;
    const resolved = resolveVariables(c, host);
    if (resolved === undefined) return undefined;
    file = resolveExecutable(resolved, host);
    if (file !== undefined) break;
  }
  if (file === undefined) return undefined;

  const rawArgs = typeof p.args === 'string' ? [p.args] : Array.isArray(p.args) ? p.args : [];
  const args: string[] = [];
  for (const a of rawArgs) {
    if (typeof a !== 'string') continue;
    const resolved = resolveVariables(a, host);
    if (resolved === undefined) return undefined;
    args.push(resolved);
  }

  const env: Record<string, string> = {};
  const unset: string[] = [];
  if (typeof p.env === 'object' && p.env !== null) {
    for (const [k, v] of Object.entries(p.env as Record<string, unknown>)) {
      if (!envName.test(k)) continue;
      if (v === null) {
        unset.push(k);
      } else if (typeof v === 'string') {
        const resolved = resolveVariables(v, host);
        if (resolved === undefined) return undefined;
        env[k] = literal(resolved);
      }
    }
  }

  // VS Code's `null` unsets a variable; `perch new --env` can only set one.
  const prefix = unset.length > 0 ? ['/usr/bin/env', ...unset.flatMap((k) => ['-u', k])] : [];
  const iconId = typeof p.icon === 'string' ? p.icon : (p.icon as { id?: unknown } | undefined)?.id;
  return {
    name,
    kind: 'shell',
    command: [...prefix, file, ...args].map(literal),
    env: Object.keys(env).length > 0 ? env : undefined,
    icon: typeof iconId === 'string' && codicon.test(iconId) ? iconId : undefined,
    color: typeof p.color === 'string' && p.color.startsWith('terminal.') ? p.color : undefined,
    display: [file, ...args].join(' '),
  };
}

/** Every profile Perch can run, in the order the settings list them. */
export function profileLaunches(profiles: unknown, host: ProfileHost): Launchable[] {
  if (typeof profiles !== 'object' || profiles === null) return [];
  const out: Launchable[] = [];
  for (const [name, value] of Object.entries(profiles as Record<string, unknown>)) {
    const launch = profileLaunch(name, value, host);
    if (launch !== undefined) out.push(launch);
  }
  return out;
}

const maxCommandName = 40;

/** A command line typed by the user, run by `/bin/sh -c`. */
export function commandLaunch(text: string): Launchable {
  const line = text.trim();
  const name = line.length > maxCommandName ? `${line.slice(0, maxCommandName - 1)}…` : line;
  return { name, kind: '', command: ['/bin/sh', '-c', literal(line)], display: line };
}
