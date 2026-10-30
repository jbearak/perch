// SPDX-License-Identifier: GPL-3.0-only

// Shared by the fidelity harness (run.mjs) and the tmux check
// (tmux-check.mjs): finding fixtures and model-dump, running model-dump
// on a fixture, and walking a fixture's actions.

import { execFileSync } from 'child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync } from 'fs';
import { tmpdir } from 'os';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';

export const here = dirname(fileURLToPath(import.meta.url));
export const repo = resolve(here, '../..');
export const fixturesDir = join(here, 'fixtures');

/// Command-line options: `--model-dump PATH`, `--only NAME` (repeatable),
/// `--xterm 5.3|5.5|6.0` (repeatable), `--keep DIR`, `--recorded DIR`.
export function options(argv) {
  const o = { only: [], xterm: [], modelDump: process.env.PERCH_MODEL_DUMP, keep: null, recorded: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--model-dump') o.modelDump = argv[++i];
    else if (a === '--only') o.only.push(argv[++i]);
    else if (a === '--xterm') o.xterm.push(argv[++i]);
    else if (a === '--keep') o.keep = argv[++i];
    else if (a === '--recorded') o.recorded = argv[++i];
    else throw new Error(`unknown option ${a}`);
  }
  o.modelDump ??= join(repo, 'target/debug/model-dump');
  if (!o.recorded && !existsSync(o.modelDump)) {
    throw new Error(`${o.modelDump} is missing: run \`cargo build -p perch-testprogs\` first`);
  }
  return o;
}

/// Every fixture: a directory under fixtures/ with a script.json.
export function fixtures(only) {
  return readdirSync(fixturesDir)
    .filter((n) => existsSync(join(fixturesDir, n, 'script.json')))
    .filter((n) => only.length === 0 || only.includes(n))
    .sort()
    .map((name) => {
      const dir = join(fixturesDir, name);
      const script = JSON.parse(readFileSync(join(dir, 'script.json'), 'utf8'));
      const stream = readFileSync(join(dir, script.stream));
      return { name, dir, script, stream };
    });
}

/// Runs model-dump on a fixture; returns its output directory and summary.
export function modelDump(opts, fx) {
  const out = mkdtempSync(join(opts.keep ?? tmpdir(), `perch-fidelity-${fx.name}-`));
  execFileSync(opts.modelDump, [join(fx.dir, 'script.json'), out], { stdio: ['ignore', 'inherit', 'inherit'] });
  const summary = JSON.parse(readFileSync(join(out, 'summary.json'), 'utf8'));
  const file = (n) => readFileSync(join(out, n));
  return { out, summary, file };
}

/// The fixture's actions as steps over the stream: `{bytes}` for output the
/// program wrote, `{resize}`, and `{restore, index}`.
export function* steps(fx) {
  let pos = 0;
  let index = 0;
  for (const a of fx.script.actions) {
    if ('feed' in a) {
      const end = a.feed === null ? fx.stream.length : Math.min(pos + a.feed, fx.stream.length);
      yield { bytes: fx.stream.subarray(pos, end) };
      pos = end;
    } else if ('fill' in a) {
      yield { bytes: Buffer.alloc(a.fill.count, a.fill.byte) };
    } else if ('repeat' in a) {
      yield { bytes: Buffer.from(a.repeat.text.repeat(a.repeat.count), 'utf8') };
    } else if ('exit_alt' in a) {
      yield { bytes: Buffer.from(`\x1b[?${a.exit_alt.mode}l${a.exit_alt.then ?? ''}`, 'utf8') };
    } else if ('resize' in a) {
      yield { resize: a.resize };
    } else if ('restore' in a) {
      yield { restore: a.restore, index: index++ };
    } else {
      throw new Error(`${fx.name}: unknown action ${JSON.stringify(a)}`);
    }
  }
}
