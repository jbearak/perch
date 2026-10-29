// SPDX-License-Identifier: GPL-3.0-only

// Writes script.json for a recording made by record.sh:
//
//   node gen/recorded-script.mjs DIR CUT NAME
//
// CUT is where the program's own exit starts, written with `\033` for ESC:
// the restore goes at its last occurrence in the stream (or at the end if
// it is empty or missing). A screen restore and a full restore are made
// there, at the recording's size, and the rest of the stream (the exit and
// what follows) is then fed to both terminals.

import { readFileSync, writeFileSync } from 'fs';
import { join } from 'path';

const [dir, cut, name] = process.argv.slice(2);
const stream = readFileSync(join(dir, 'stream.bin'));
const marker = Buffer.from(cut.replaceAll('\\033', '\x1b'), 'latin1');
let at = marker.length ? stream.lastIndexOf(marker) : -1;
if (at < 0) at = stream.length;

// Named assertions on the full restore, per recording.
const ASSERT = {
  'r-prompt': [{ name: 'cursor after the R prompt', cursor_x: 2 }, { name: 'output after R exits', contains: 'after R', after: true }],
  'zsh-right-prompt': [
    { name: 'cursor after the left prompt', cursor_x: 10 },
    { name: 'right prompt kept', contains: '[1] right' },
  ],
  'vim-in-out': [
    { name: 'in vim on the alternate screen', buffer: 'alternate' },
    { name: 'back on the primary screen after vim', buffer: 'normal', after: true },
    { name: 'output after vim', contains: 'more output', after: true },
  ],
  less: [
    { name: 'in less on the alternate screen', buffer: 'alternate' },
    { name: 'back on the primary screen after less', buffer: 'normal', after: true },
  ],
};

const restore = (mode, assert) => ({ restore: { mode, size: [80, 24], scrollback: 1000, ...(assert ? { assert } : {}) } });
const actions = [{ feed: at }, restore('screen'), restore('full', ASSERT[name])];
if (at < stream.length) actions.push({ feed: null });

const script = {
  description: `${name}: recorded by record.sh at 80x24, restored before the program's own exit`,
  recorded_by: 'record.sh',
  size: [80, 24],
  scrollback: 1000,
  stream: 'stream.bin',
  actions,
};
const path = join(dir, 'script.json');
let known;
try {
  known = JSON.parse(readFileSync(path, 'utf8')).known;
} catch {}
// Keep known differences a maintainer recorded by hand.
if (known) script.known = known;
writeFileSync(path, `${JSON.stringify(script, null, 2)}\n`);
