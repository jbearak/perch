// SPDX-License-Identifier: GPL-3.0-only

// The programs "New Persistent Terminal..." offers when it finds them on
// `PATH`: common REPLs, each run bare in a new session.
import { Launchable, literal, ProfileHost, resolveExecutable } from './profiles';

interface Program {
  label: string;
  kind: string;
  /** Names to look for on `PATH`, in order. */
  names: string[];
}

const programs: readonly Program[] = [
  { label: 'Python', kind: 'python', names: ['python3', 'python'] },
  { label: 'IPython', kind: 'python', names: ['ipython3', 'ipython'] },
  { label: 'R', kind: 'r', names: ['R'] },
  { label: 'radian', kind: 'r', names: ['radian'] },
  { label: 'Julia', kind: 'julia', names: ['julia'] },
  { label: 'Node.js', kind: 'node', names: ['node'] },
];

export interface ProgramHost extends ProfileHost {
  platform: NodeJS.Platform;
  exists(file: string): boolean;
}

/**
 * macOS ships `/usr/bin/python3` as a stub that offers to install the
 * developer tools; it runs Python only once they are installed.
 */
function isStub(file: string, host: ProgramHost): boolean {
  if (host.platform !== 'darwin' || (file !== '/usr/bin/python3' && file !== '/usr/bin/python')) return false;
  return !host.exists('/Library/Developer/CommandLineTools') && !host.exists('/Applications/Xcode.app');
}

/** The programs found on `PATH`, in the order of the list. */
export function programLaunches(host: ProgramHost): Launchable[] {
  const h: ProfileHost = { ...host, isExecutable: (f) => !isStub(f, host) && host.isExecutable(f) };
  const out: Launchable[] = [];
  for (const p of programs) {
    for (const name of p.names) {
      const file = resolveExecutable(name, h);
      if (file === undefined) continue;
      out.push({ name: p.label, kind: p.kind, command: [literal(file)], display: file });
      break;
    }
  }
  return out;
}
