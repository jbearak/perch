// SPDX-License-Identifier: GPL-3.0-only

// Writes the synthetic fixtures under tests/fidelity/fixtures/: byte
// streams that exercise what programs rarely do on cue (kitty stacks,
// 2027, resizes, DECSTR, RIS, oversized sequences, split UTF-8, the alternate
// screen modes in every order), each with its action script. Run it after
// changing a fixture here, and commit what it writes: the harness and CI
// only replay the files.
//
//   node tests/fidelity/gen/synthetic.mjs

import { mkdirSync, writeFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const out = join(dirname(fileURLToPath(import.meta.url)), '..', 'fixtures');

const ESC = '\x1b';
const CSI = `${ESC}[`;
const OSC = `${ESC}]`;
const ST = `${ESC}\\`;

/// A fixture: `parts` are strings (UTF-8) or Buffers, fed in order; a
/// `{ action }` part ends the bytes so far with a feed and adds the action.
function fixture(name, { description, size = [80, 24], scrollback = 1000, known, knownTmux, parts }) {
  const chunks = [];
  const actions = [];
  let pending = 0;
  const flush = () => {
    if (pending > 0) actions.push({ feed: pending });
    pending = 0;
  };
  for (const p of parts) {
    if (typeof p === 'string' || Buffer.isBuffer(p)) {
      const b = Buffer.isBuffer(p) ? p : Buffer.from(p, 'utf8');
      chunks.push(b);
      pending += b.length;
    } else {
      flush();
      actions.push(p);
    }
  }
  flush();
  const dir = join(out, name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'stream.bin'), Buffer.concat(chunks));
  const script = { description, generated_by: 'gen/synthetic.mjs', size, scrollback, stream: 'stream.bin', actions };
  if (known) script.known = known;
  if (knownTmux) script.known_tmux = knownTmux;
  writeFileSync(join(dir, 'script.json'), `${JSON.stringify(script, null, 2)}\n`);
}

const restore = (o = {}) => ({ restore: { mode: 'full', size: [80, 24], scrollback: 1000, ...o } });
const lines = (n, f) => Array.from({ length: n }, (_, i) => f(i)).join('\r\n');

// ---------------------------------------------------------------------------
// Kitty keyboard stacks.

fixture('kitty-stacks', {
  description: 'kitty stacks on both screens, with zero entries pushed, then pushes and pops after the restore',
  parts: [
    'primary\r\n',
    `${CSI}>1u${CSI}>0u${CSI}>3u${CSI}=5;2u`,
    `${CSI}?1049h${CSI}>5u${CSI}>0u${CSI}>7u alternate`,
    restore({
      kitty4: true,
      assert: [
        { name: 'primary stack, zeros kept', kitty: { screen: 'primary', flags: [0, 0, 0, 0, 0, 1, 0, 7] } },
        { name: 'alternate stack', kitty: { screen: 'alternate', flags: [0, 0, 0, 0, 0, 5, 0, 7] } },
        { name: 'on the alternate screen', buffer: 'alternate' },
      ],
    }),
    `${CSI}<1u${CSI}>9u`,
    { exit_alt: { mode: 1049, then: 'back\r\n' } },
    `${CSI}<2u${CSI}>2u`,
    restore({ mode: 'screen', kitty4: true }),
    `${CSI}<1u`,
  ],
});

fixture('kitty-wraparound', {
  description: 'eleven pushes wrap the 8-entry ring; pops after the restore expose older entries',
  parts: [
    'wrap\r\n',
    Array.from({ length: 11 }, (_, i) => `${CSI}>${i + 1}u`).join(''),
    restore({ kitty4: true, assert: [{ name: 'ring after wraparound', kitty: { screen: 'primary', flags: [4, 5, 6, 7, 8, 9, 10, 11] } }] }),
    `${CSI}<3u${CSI}>1u${CSI}<1u${CSI}<1u`,
  ],
});

fixture('kitty-zero', {
  description: 'only zeros pushed: the restore writes no stack, and later pops stay at zero',
  parts: [
    `${CSI}>0u${CSI}>0u${CSI}>0u zeros`,
    restore({ kitty4: true, assert: [{ name: 'all zero', kitty: { screen: 'primary', flags: [0, 0, 0, 0, 0, 0, 0, 0] } }] }),
    `${CSI}<1u${CSI}>4u`,
  ],
});

// ---------------------------------------------------------------------------
// 2027, widths, hyperlinks, DEC special graphics.

fixture('mode-2027', {
  description: 'grapheme clustering on with width-sensitive text, so a late 2027 would misplace the cursor',
  parts: [
    `${CSI}?2027h`,
    'family 👨‍👩‍👧 heart ❤️ flag 🇫🇷 skin 👍🏽 end\r\n',
    'x👨‍👩‍👧y',
    restore({ assert: [{ name: 'cursor after the cluster', cursor: [4, 1] }] }),
    ' more 👨‍👩‍👧\r\n',
  ],
  known: [
    { match: 'cursor: ', where: '(xterm', reason: 'xterm.js has no mode 2027, so A lays the clusters out differently; the perch-vt oracle and the named assertion check the cursor' },
    { match: 'normal line 1', where: 'after (', reason: 'xterm.js has no mode 2027, so A lays the clusters out differently' },
  ],
});

fixture('wide-graphemes', {
  description: 'CJK, emoji, combining marks, and a wide character that does not fit at the right edge',
  size: [20, 6],
  parts: [
    '中文字符 漢字 é ñ ä\r\n',
    '😀😃😄 ok\r\n',
    'abcdefghijklmnopqrs中x\r\n',
    'abcdefghijklmnopqr中',
    restore({ size: [20, 6], assert: [{ name: 'a wide character that did not fit moved to the next row', line: [3, '中x'] }, { name: 'cursor waits to wrap after the wide character at the edge', cursor: [20, 4] }] }),
    'z\r\n',
  ],
});

fixture('hyperlinks', {
  description: 'OSC 8 links with and without ids, one wrapping across rows, one still open',
  size: [30, 6],
  parts: [
    `${OSC}8;;https://example.com/a${ST}plain link${OSC}8;;${ST}\r\n`,
    `${OSC}8;id=x;https://example.com/b${ST}a link with an id that wraps across rows${OSC}8;;${ST}\r\n`,
    `${CSI}1;4:3;58;5;2mstyled${CSI}0m ${OSC}8;;https://example.com/open${ST}still open`,
    restore({ size: [30, 6], assert: [{ name: 'open link continues', line: [3, 'styled still open and more'], after: true }] }),
    ' and more',
  ],
});

fixture('dec-special-margin', {
  description: 'DEC special graphics up to the right margin, leaving the cursor waiting to wrap',
  size: [20, 4],
  parts: [
    `${ESC}(0lqqqqqqqqqqqqqqqqqk\r\n`,
    `x ${ESC}(B text ${ESC}(0x\r\n`,
    `mqqqqqqqqqqqqqqqqqqj`,
    restore({ size: [20, 4], assert: [{ name: 'pending wrap at the margin', cursor: [20, 2] }, { name: 'next print wraps', line: [3, 'wrapped'], after: true }] }),
    `${ESC}(Bwrapped`,
  ],
});

fixture('progress-bar', {
  description: 'a progress bar redrawn in place with carriage returns and erase-line',
  parts: [
    'downloading\r\n',
    ...Array.from({ length: 21 }, (_, i) => `\r${CSI}K[${'#'.repeat(i * 2)}${' '.repeat(40 - i * 2)}] ${i * 5}%`),
    restore({ assert: [{ name: 'bar at 100%', line: [1, `[${'#'.repeat(40)}] 100%`] }, { name: 'bar replaced', line: [1, 'done'], after: true }] }),
    `\r${CSI}Kdone\r\n`,
  ],
});

// ---------------------------------------------------------------------------
// Resets.

fixture('decstr', {
  description: 'DECSTR after modes, margins, pen and origin mode',
  knownTmux: [{ restores: [0], reason: 'tmux does not implement DECSTR, so the pen, character set, margins and origin mode stay set in the original' }],
  parts: [
    'before\r\n',
    `${CSI}?6h${CSI}5;10r${CSI}?1h${CSI}4h${CSI}1;7m${ESC}(0${CSI}?25l`,
    `${CSI}!p`,
    'after DECSTR\r\n',
    restore({ assert: [{ name: 'cursor visible again', contains: 'after DECSTR' }] }),
    'more\r\n',
  ],
});

fixture('ris', {
  description: 'RIS after the alternate screen, kitty flags, palette and title',
  parts: [
    'gone\r\n',
    `${OSC}4;1;rgb:12/34/56${ST}${OSC}2;title${ST}${CSI}>3u${CSI}?1049h${CSI}?2004halt`,
    `${ESC}c`,
    'after RIS\r\n',
    restore({ assert: [{ name: 'screen cleared by RIS', line: [0, 'after RIS'] }, { name: 'primary screen after RIS', buffer: 'normal' }] }),
    'more\r\n',
  ],
});

// ---------------------------------------------------------------------------
// Unfinished sequences and bytes.

fixture('split-utf8', {
  description: 'restores while a UTF-8 character is split, so the held bytes follow the restore',
  parts: [
    Buffer.from('caf\xc3', 'latin1'),
    restore({ assert: [{ name: 'held character completes', line: [0, 'café '], after: true }] }),
    Buffer.from('\xa9 \xe4\xb8', 'latin1'),
    restore({ mode: 'screen' }),
    Buffer.from('\xad\xe6\x96\x87\r\n', 'latin1'),
  ],
});

fixture('partial-sequences', {
  description: 'restores in the middle of a CSI, an OSC and a DCS',
  parts: [
    'start ',
    `${CSI}3`,
    restore({ assert: [{ name: 'held CSI completes red', line: [0, 'start red '], after: true }] }),
    `1mred${CSI}0m ${OSC}2;ti`,
    restore({}),
    `tle${ST} ${ESC}Pq#0;2;0;0;0`,
    restore({}),
    `${ST}end\r\n`,
  ],
});

fixture('invalid-bytes', {
  description: 'invalid UTF-8, stray continuation bytes and C1 bytes at ground',
  parts: [Buffer.from('a\xffb\xc3(c\x80d\xe2\x82e\x9bf\xc2\x9b31mg\r\n', 'latin1'), restore({ assert: [{ name: 'model replaces invalid bytes', model_line: ['primary', 0, 'a\ufffdb\ufffd(c\ufffdd\ufffde\ufffdf31mg'] }] }), 'h\r\n'],
  knownTmux: [{ restores: [0], reason: 'tmux decodes invalid UTF-8 and UTF-8 encoded C1 its own way' }],
  known: [{ match: 'normal line', reason: 'Ghostty and xterm.js decode invalid UTF-8 and UTF-8 encoded C1 differently; not covered by the contract' }],
});

// ---------------------------------------------------------------------------
// Oversized sequences, built from `fill` so no large file is committed.

const MiB17 = 17 * 1024 * 1024;

fixture('oversized-apc', {
  description: 'an APC over 16 MiB is never given to the model; a client attaching in its middle gets none of the rest',
  parts: [
    'before\r\n',
    `${ESC}_G`,
    { fill: { byte: 0x41, count: MiB17 } },
    restore({ assert: [{ name: 'text after the string', line: [1, 'after'], after: true }] }),
    { fill: { byte: 0x41, count: 1024 } },
    `${ST}after\r\n`,
    restore({}),
  ],
});

fixture('oversized-csi', {
  description: 'a CSI over 16 MiB is streamed to the model, which is partial until RIS',
  parts: [
    'before\r\n',
    `${CSI}1`,
    { fill: { byte: 0x31, count: MiB17 } },
    restore({}),
    'm\r\nafter\r\n',
    restore({}),
    `${ESC}cclean\r\n`,
    restore({}),
  ],
});

// ---------------------------------------------------------------------------
// Sizes and history.

fixture('resize-200-80-200', {
  description: 'long wrapped lines at 200 columns, restored at 80 and at 200 again',
  size: [200, 20],
  parts: [
    lines(40, (i) => `${i} ${'word '.repeat(10 + (i % 30))}`),
    '\r\n',
    { resize: [80, 20] },
    restore({ size: [80, 20], assert: [{ name: 'cursor at the left after the reflow', cursor_x: 0 }] }),
    'at 80\r\n',
    { resize: [200, 20] },
    restore({ size: [200, 20] }),
    'at 200\r\n',
  ],
});

fixture('history-client-limit', {
  description: 'more history than the client keeps: B gets the newest rows that fit',
  scrollback: 5000,
  parts: [lines(3000, (i) => `line ${i}`), '\r\n', restore({ scrollback: 500, assert: [{ name: 'client scrollback full', scrollback: 500 }] })],
});

fixture('history-4mib', {
  description: 'history over the 4 MiB cap, every cell in its own RGB color',
  size: [200, 24],
  scrollback: 10000,
  parts: [
    { repeat: { text: `${Array.from({ length: 200 }, (_, i) => `${CSI}38;2;${i};${(i * 7) % 256};${(i * 13) % 256}m${String.fromCharCode(97 + (i % 26))}`).join('')}${CSI}0m\r\n`, count: 1500 } },
    'tail\r\n',
    restore({ size: [200, 24], scrollback: 10000, assert: [{ name: 'history cut to 4 MiB', scrollback_below: 1500 }] }),
  ],
});

fixture('history-blank-tail', {
  description: 'history ending in blank lines, kept with no row dropped or added at the screen boundary',
  size: [40, 5],
  parts: [lines(12, (i) => (i < 6 ? `text ${i}` : '')), restore({ size: [40, 5], assert: [{ name: 'blank history rows kept', scrollback: 7 }] }), 'after\r\n'],
});

fixture('restore-byte-cap', {
  description: 'a restore past its byte limit falls back to the prologue, the notice and the nudge',
  parts: [`${CSI}?2004h`, lines(20, (i) => `${CSI}3${i % 8}mrow ${i}`), restore({ byte_cap: 256 })],
});

// ---------------------------------------------------------------------------
// The alternate screen, entered and left with 47, 1047 and 1049 in each
// order. The primary screen has a saved cursor; the alternate screen has
// its own.

// Restore 1 is made on the primary screen and the program then re-enters
// the alternate screen.
const HIDDEN_ALT =
  'a restore made on the primary screen does not carry the hidden alternate screen (its contents and saved cursor), which the program finds again on re-entering it; the design restores the alternate screen only while the program is on it';

for (const enter of [47, 1047, 1049]) {
  for (const exit of [47, 1047, 1049]) {
    fixture(`alt-${enter}-${exit}`, {
      description: `enter the alternate screen with ${enter}, leave with ${exit}, then re-enter with ${enter}`,
      size: [40, 8],
      parts: [
        'primary one\r\nprimary two',
        `${CSI}3;5H${CSI}32m${ESC}7${CSI}0m${CSI}6;10H`,
        `${CSI}?${enter}h`,
        `${CSI}H${CSI}2Jalt screen${CSI}2;2H${CSI}35m${ESC}7${CSI}0m${CSI}4;4Hcursor`,
        restore({ size: [40, 8], assert: [{ name: 'primary saved cursor', saved: { screen: 'primary', at: savedPrimary(enter) } }] }),
        { exit_alt: { mode: exit, then: 'back' } },
        `${ESC}8x`,
        restore({ size: [40, 8], mode: 'screen' }),
        `${CSI}?${enter}hagain${ESC}8y`,
        restore({ size: [40, 8] }),
        { exit_alt: { mode: exit, then: '' } },
        `${ESC}8z`,
      ],
      knownTmux: [
        {
          restores: [1, 2],
          reason: 'tmux keeps one saved cursor for both screens, so ESC 8 after the program leaves the alternate screen goes elsewhere in the original; Ghostty and xterm.js keep one per screen and agree with the restore',
        },
      ],
      known: [
        { match: 'alternate', where: 'restore 1 after', reason: HIDDEN_ALT },
        { match: 'cursor: ', where: 'restore 1 after', reason: HIDDEN_ALT },
        { match: 'savedAlternate', where: 'restore 1 after', reason: HIDDEN_ALT },
        {
          match: 'alternate line',
          where: 'restore 2 (xterm',
          reason: 'xterm.js clears its alternate buffer when the program leaves it; Ghostty keeps it, so re-entering with 47 shows the old contents in the model and the restore',
        },
      ],
    });
  }
}

function savedPrimary(enter) {
  // 1049 saves the cursor on entry, overwriting the ESC 7 at 3;5.
  return enter === 1049 ? [9, 5] : [4, 2];
}

// ---------------------------------------------------------------------------
// Shells and line editors, synthetic. Recorded programs are in fixtures too
// (record.sh); these cover what this machine cannot record.

fixture('reedline-redraw', {
  description: 'a reedline-style prompt: bracketed paste, cursor queries, full-line redraws with a right prompt and a hint',
  parts: [
    `${CSI}?2004h${CSI}6n`,
    `${CSI}?25l${CSI}1;1H${CSI}J${CSI}32m~/thesis${CSI}0m ${CSI}1;72H${CSI}2m12:00:01${CSI}0m${CSI}1;10H> ${CSI}?25h`,
    `${CSI}?25l${CSI}1;10H${CSI}K> x <- 1${CSI}1;72H${CSI}2m12:00:01${CSI}0m${CSI}1;18H${CSI}?25h`,
    `${CSI}?25l${CSI}1;18H${CSI}90m:10${CSI}0m${CSI}1;18H${CSI}?25h`,
    restore({ assert: [{ name: 'cursor before the hint', cursor: [17, 0] }, { name: 'bracketed paste on', model_mode: ['?2004', true] }] }),
    `\r\n${CSI}?2004l[1] 1\r\n${CSI}?2004h${CSI}6n${CSI}32m~/thesis${CSI}0m > `,
  ],
});

fixture('fish-right-prompt', {
  description: 'a fish-style right prompt drawn with cursor movement after the left prompt (fish is not installed here, so this is synthetic)',
  parts: [
    `${OSC}133;A${ST}${CSI}1mjmb${CSI}0m@host ~/p> ${CSI}s${CSI}66G${CSI}33m(main) 0.2s${CSI}0m${CSI}u`,
    'echo hi',
    `\r\n${OSC}133;C${ST}hi\r\n${OSC}133;D;0${ST}`,
    `${OSC}133;A${ST}${CSI}1mjmb${CSI}0m@host ~/p> ${CSI}s${CSI}70G${CSI}33m(main)${CSI}0m${CSI}u`,
    restore({ assert: [{ name: 'right prompt drawn', line: [2, `jmb@host ~/p>${' '.repeat(56)}(main)`] }, { name: 'cursor after the left prompt', cursor: [14, 2] }] }),
    restore({ mode: 'screen' }),
    'ls\r\n',
  ],
});

console.log(`synthetic fixtures written to ${out}`);
