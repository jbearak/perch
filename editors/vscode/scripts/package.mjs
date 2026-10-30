// SPDX-License-Identifier: GPL-3.0-only

// Packages one VSIX with vsce from a staging copy of the extension, so the
// working tree's bin/ (used by the tests) is left alone, then checks it with
// check-vsix.mjs. Needs `npm run build` first, and vsce from release/
// (`npm ci --omit=optional --ignore-scripts` there).
//
//   node scripts/package.mjs --target darwin-arm64 --binary path/to/perch --out dir
//   node scripts/package.mjs --target universal --out dir
//
// The staging copy adds the repository's LICENSE, NOTICE and README.md, and
// for a platform target the binary as bin/<os>-<arch>/perch (mode 0755).
import { execFileSync } from 'child_process';
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'fs';
import { tmpdir } from 'os';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';
import { checkVsix } from './check-vsix.mjs';
import { checkTarget, targets, universal, vsixName } from './targets.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const repo = join(root, '../..');

const args = parseArgs(process.argv.slice(2));
const target = checkTarget(args.target);
if ((target === universal) !== (args.binary === undefined)) {
  fail(target === universal ? 'the universal VSIX takes no --binary' : `--target ${target} needs --binary`);
}
const vsce = join(root, 'release/node_modules/.bin/vsce');
if (!existsSync(vsce)) fail('no vsce: run `npm ci --omit=optional --ignore-scripts` in editors/vscode/release');
if (!existsSync(join(root, 'dist/extension.js'))) fail('no dist/extension.js: run `npm run build` first');

const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;
const out = resolve(args.out ?? '.');
mkdirSync(out, { recursive: true });
const vsix = join(out, vsixName(target, version));

const stage = mkdtempSync(join(tmpdir(), 'perch-package-'));
try {
  for (const entry of ['package.json', '.vscodeignore', 'dist', 'media']) {
    cpSync(join(root, entry), join(stage, entry), { recursive: true });
  }
  for (const file of ['LICENSE', 'NOTICE', 'README.md']) copyFileSync(join(repo, file), join(stage, file));
  if (target !== universal) {
    const dir = join(stage, 'bin', targets[target].dir);
    mkdirSync(dir, { recursive: true });
    copyFileSync(resolve(args.binary), join(dir, 'perch'));
    chmodSync(join(dir, 'perch'), 0o755);
  }
  const vsceArgs = ['package', '--no-dependencies', '--out', vsix];
  if (target !== universal) vsceArgs.push('--target', target);
  execFileSync(vsce, vsceArgs, { cwd: stage, stdio: 'inherit' });
} finally {
  rmSync(stage, { recursive: true, force: true });
}

checkVsix(vsix, target, version);
console.log(`package: ${vsix}`);

function parseArgs(argv) {
  const o = {};
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (!['--target', '--binary', '--out'].includes(flag) || value === undefined) {
      fail(`usage: package.mjs --target <target> [--binary <perch>] [--out <dir>] (at \`${flag}\`)`);
    }
    o[flag.slice(2)] = value;
  }
  if (o.target === undefined) fail('--target is required');
  return o;
}

function fail(message) {
  console.error(`package: ${message}`);
  process.exit(1);
}
