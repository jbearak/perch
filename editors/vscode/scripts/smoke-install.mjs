// SPDX-License-Identifier: GPL-3.0-only

// Installs VSIXs with the `code` CLI of a VS Code downloaded by
// @vscode/test-electron, each into an empty temporary --extensions-dir, as a
// user's install would:
//
// - this host's own platform VSIX: installs, and its binary runs `new -d`,
//   `list` and `kill` (scripts/release-smoke.sh), and on macOS passes
//   `codesign --verify`;
// - the universal VSIX: installs, with no binary;
// - with --foreign <target>: that VSIX is refused, as VS Code refuses a VSIX
//   for another platform.
//
//   node scripts/smoke-install.mjs --vsix-dir dir --target darwin-arm64 [--foreign linux-x64]
//
// PERCH_VSCODE_VERSION picks the VS Code (default stable).
import { execFileSync, spawnSync } from 'child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'fs';
import { tmpdir } from 'os';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';
import { downloadAndUnzipVSCode, resolveCliPathFromVSCodeExecutablePath } from '@vscode/test-electron';
import { checkTarget, targets, universal, vsixName } from './targets.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const smoke = join(root, '../../scripts/release-smoke.sh');
const extensionId = 'jbearak.perch';
const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;

class Smoke {
  constructor(code, scratch) {
    this.code = code;
    this.scratch = scratch;
  }

  /** Runs the `code` CLI with extensions and user data directories under `name`. */
  cli(name, cliArgs) {
    const dir = join(this.scratch, name);
    const r = spawnSync(
      this.code,
      [`--extensions-dir=${join(dir, 'ext')}`, `--user-data-dir=${join(dir, 'user')}`, ...cliArgs],
      { encoding: 'utf8' },
    );
    if (r.error) throw new Error(`running ${this.code}: ${r.error.message}`);
    return r;
  }

  /** Installs a VSIX into a fresh extensions directory; returns the extension's directory. */
  install(file, name) {
    const r = this.cli(name, ['--install-extension', file]);
    if (r.status !== 0) throw new Error(`installing ${file} failed:\n${r.stderr}${r.stdout}`);
    const listed = this.cli(name, ['--list-extensions', '--show-versions']).stdout;
    if (!listed.includes(`${extensionId}@${version}`)) throw new Error(`${file}: not listed after install:\n${listed}`);
    const ext = join(this.scratch, name, 'ext');
    const installed = readdirSync(ext).filter((d) => d.startsWith(`${extensionId}-${version}`));
    if (installed.length !== 1) throw new Error(`${file}: expected one installed directory, found ${JSON.stringify(installed)}`);
    console.log(`smoke-install: installed ${file} as ${installed[0]}`);
    return join(ext, installed[0]);
  }

  /** Throws unless VS Code refuses the VSIX. */
  refuse(file, name) {
    const r = this.cli(name, ['--install-extension', file]);
    const listed = this.cli(name, ['--list-extensions']).stdout;
    if (r.status === 0 || listed.includes(extensionId)) throw new Error(`${file}: VS Code installed another platform's VSIX`);
    console.log(`smoke-install: ${file} refused: ${(r.stderr + r.stdout).trim().split('\n').pop()}`);
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const target = checkTarget(args.target);
  if (target === universal) throw new Error("--target is this host's platform target, not universal");
  const vsix = (t) => join(resolve(args['vsix-dir']), vsixName(t, version));

  const code = resolveCliPathFromVSCodeExecutablePath(
    await downloadAndUnzipVSCode(process.env.PERCH_VSCODE_VERSION ?? 'stable'),
  );
  const s = new Smoke(code, mkdtempSync(join(tmpdir(), 'perch-install-')));
  try {
    const own = s.install(vsix(target), 'own');
    const binary = join(own, 'bin', targets[target].dir, 'perch');
    if (!existsSync(binary)) throw new Error(`${target}: no binary at ${binary}`);
    if (process.platform === 'darwin') execFileSync('codesign', ['--verify', '--verbose', binary], { stdio: 'inherit' });
    execFileSync('sh', [smoke, binary], { stdio: 'inherit' });

    const any = s.install(vsix(universal), 'universal');
    if (existsSync(join(any, 'bin'))) throw new Error('universal: installed with a bin/ directory');
    console.log('smoke-install: universal VSIX installed, no binary');

    if (args.foreign !== undefined) s.refuse(vsix(checkTarget(args.foreign)), 'foreign');
    console.log('smoke-install: ok');
  } finally {
    rmSync(s.scratch, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  const o = {};
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (!['--vsix-dir', '--target', '--foreign'].includes(flag) || value === undefined) {
      throw new Error(`usage: smoke-install.mjs --vsix-dir <dir> --target <target> [--foreign <target>] (at \`${flag}\`)`);
    }
    o[flag.slice(2)] = value;
  }
  if (o['vsix-dir'] === undefined || o.target === undefined) throw new Error('--vsix-dir and --target are required');
  return o;
}

main().catch((err) => {
  console.error(`smoke-install: ${err.message}`);
  process.exit(1);
});
