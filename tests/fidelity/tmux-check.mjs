// SPDX-License-Identifier: GPL-3.0-only

// tmux as a second terminal for text and colors. For each restore that the
// harness checks against xterm.js at the same size, two tmux panes of that
// size run `cat original.bin` and `cat restore.bin`, and `capture-pane -p
// -e` of each (scrollback included for a full restore, the screen alone for
// a screen restore) must match for text and SGR.
//
//   node tests/fidelity/tmux-check.mjs [--only NAME] [--model-dump PATH]
//
// A fixture's `known_tmux` lists restores where tmux itself differs from
// Ghostty and xterm.js, each with the reason.

import { execFileSync } from 'child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { fixtures, modelDump, options, steps } from './lib.mjs';

const opts = options(process.argv.slice(2));
// tmux puts its socket here: keep the path short.
const dir = mkdtempSync(join(process.env.TMUX_CHECK_DIR ?? tmpdir(), 'ptmux-'));
const sock = join(dir, 's');
const conf = join(dir, 'tmux.conf');
writeFileSync(conf, 'set -g status off\nset -g history-limit 100000\nset -g remain-on-exit on\n');

const tmux = (...args) => execFileSync('tmux', ['-S', sock, '-f', conf, ...args], { encoding: 'utf8', maxBuffer: 1 << 28 });

let pane = 0;
function capture(file, cols, rows, full) {
  const name = `p${pane++}`;
  tmux('new-session', '-d', '-s', name, '-x', String(cols), '-y', String(rows),
    // No echo: tmux answers the stream's queries into the pane's input.
    `stty -echo; cat '${file}'; tmux -S '${sock}' wait-for -S ${name}; exec sleep 100000`);
  tmux('wait-for', name);
  const out = tmux('capture-pane', '-p', '-e', '-t', name, ...(full ? ['-S', '-', '-E', '-'] : []));
  tmux('kill-session', '-t', name);
  return out.replace(/\n+$/, '').split('\n').map(acs);
}

// tmux keeps DEC special graphics as an attribute (shown as SO ... SI in
// a capture); the restore writes the Unicode characters Ghostty stored.
const ACS = { j: '┘', k: '┐', l: '┌', m: '└', n: '┼', q: '─', t: '├', u: '┤', v: '┴', w: '┬', x: '│', a: '▒', '`': '◆', f: '°', g: '±', '~': '·', o: '⎺', s: '⎽', y: '≤', z: '≥', '{': 'π', '|': '≠', '}': '£' };
function acs(line) {
  return line.replace(/\x0e([^\x0f]*)(\x0f|$)/g, (_, body) =>
    body
      .split(/(\x1b\[[0-9;]*m)/)
      .map((part, i) => (i % 2 ? part : [...part].map((c) => ACS[c] ?? c).join('')))
      .join(''),
  );
}

let failed = 0;
let checked = 0;
try {
  for (const fx of fixtures(opts.only)) {
    const dumped = modelDump(opts, fx);
    let original = [];
    let resized = false;
    for (const step of steps(fx)) {
      if (step.bytes) original.push(Buffer.from(step.bytes));
      else if (step.resize) resized = true;
      else if (step.restore) {
        const r = dumped.summary.restores[step.index];
        const [cols, rows] = r.size;
        if (resized || r.partial || cols !== fx.script.size[0] || rows !== fx.script.size[1]) continue;
        const a = join(dir, 'original.bin');
        writeFileSync(a, Buffer.concat(original));
        const b = join(dir, 'restore.bin');
        writeFileSync(b, dumped.file(`restore-${step.index}.bin`));
        const full = r.mode === 'full';
        let la = capture(a, cols, rows, full);
        let lb = capture(b, cols, rows, full);
        // History limited by the client: the newest part of A's.
        if (lb.length < la.length) la = la.slice(la.length - lb.length);
        const diffs = [];
        if (la.length !== lb.length) diffs.push(`${la.length} lines != ${lb.length}`);
        for (let i = 0; i < Math.min(la.length, lb.length); i++) {
          if (la[i] !== lb[i]) diffs.push(`line ${i}: ${JSON.stringify(la[i])} != ${JSON.stringify(lb[i])}`);
        }
        const known = (fx.script.known_tmux ?? []).find((k) => k.restores.includes(step.index));
        checked++;
        if (diffs.length && !known) {
          failed++;
          console.log(`FAIL ${fx.name} restore ${step.index} (${r.mode})`);
          for (const d of diffs.slice(0, 6)) console.log(`       ${d.slice(0, 300)}`);
        } else {
          console.log(`ok   ${fx.name} restore ${step.index} (${r.mode})${known ? ` known: ${known.reason}` : ''}`);
        }
      }
    }
    rmSync(dumped.out, { recursive: true, force: true });
  }
} finally {
  try {
    tmux('kill-server');
  } catch {}
  rmSync(dir, { recursive: true, force: true });
}
if (failed) {
  console.log(`tmux-check: ${failed} of ${checked} restores differ`);
  process.exit(1);
}
console.log(`tmux-check: ok (${checked} restores)`);
