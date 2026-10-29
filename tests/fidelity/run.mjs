// SPDX-License-Identifier: GPL-3.0-only

// The restore fidelity harness. For each committed fixture it runs
// `model-dump` (the gate and perch-vt, as the supervisor will) and checks
// every restore against these oracles, in three headless xterm.js versions:
//
// - xterm (same size): terminal A gets the program's output, B the restore.
//   Both buffers, scrollback included, must match cell for cell
//   (characters, width, colors, attributes, underline style and color,
//   hyperlinks, wrapping), with the cursor, pending wrap, saved cursors and
//   IModes; again after the output that follows (the program's own exit
//   from the alternate screen, more output). Where history is limited, B's
//   scrollback must be the newest part of A's. A screen restore leaves the
//   scrollback B already had.
// - model: after a width change Ghostty and xterm.js reflow differently,
//   so B's rows are checked against the model's own rows.
// - fallback: a partial-model restore (or one past the byte cap) is the
//   prologue, the notice and the nudge, and nothing else.
// - perch-vt: model-dump fed the restore to a second perch-vt and compared
//   kitty stacks, saved cursors, modes and palette. Self-referential: both
//   sides are Ghostty.
// - hostile: every restore is also made into a client left in the
//   alternate screen with margins, insert mode, an open OSC 8 and a
//   half-written CSI.
// - kitty4: a client whose kitty stack holds 4 entries keeps the newest
//   ones and then behaves as one fed the original output.
//
// Fixtures may list `known` differences, each with a reason: a difference
// containing `match` (and, if given, found where `where` says) is reported
// but does not fail.

Object.defineProperty(globalThis, 'navigator', { value: undefined, configurable: true });

import { rmSync } from 'fs';
import { createRequire } from 'module';
import { fixtures, here, modelDump, options, steps } from './lib.mjs';

const require = createRequire(import.meta.url);
const VERSIONS = ['5.3', '5.5', '6.0'];

function load(version) {
  const { Terminal } = require(`xterm-headless-${version}`);
  const { Unicode11Addon } = require(`xterm-unicode11-${version}`);
  return (cols, rows, scrollback) => {
    const t = new Terminal({ cols, rows, scrollback, allowProposedApi: true });
    t.loadAddon(new Unicode11Addon());
    t.unicode.activeVersion = '11';
    return t;
  };
}

const write = (t, bytes) => new Promise((r) => (bytes.length ? t.write(bytes, r) : r()));

// ---------------------------------------------------------------------------
// Snapshots of an xterm.js terminal.

function cellKey(t, c) {
  const ext = c.extended;
  let link = '';
  if (ext?.urlId) {
    const d = t._core._oscLinkService.getLinkData(ext.urlId);
    link = d ? `${d.id ?? ''};${d.uri}` : '?';
  }
  const flags = [
    c.isBold(), c.isDim(), c.isItalic(), c.isUnderline(), c.isBlink(),
    c.isInverse(), c.isInvisible(), c.isStrikethrough(), c.isOverline?.() ?? 0,
  ].map((f) => (f ? 1 : 0)).join('');
  const ul = c.isUnderline() ? `${ext?.underlineStyle ?? 1}:${ext?.underlineColor ?? 0}` : '';
  return `${c.getChars()}|${c.getWidth()}|${c.getFgColorMode()}:${c.getFgColor()}|${c.getBgColorMode()}:${c.getBgColor()}|${flags}|${ul}|${link}`;
}

function lines(t, buf) {
  const out = [];
  const cols = t.cols;
  for (let y = 0; y < buf.length; y++) {
    const line = buf.getLine(y);
    const cells = [];
    for (let x = 0; x < cols; x++) cells.push(cellKey(t, line.getCell(x)));
    out.push({ wrapped: line.isWrapped, text: line.translateToString(true), cells });
  }
  return out;
}

// xterm.js saves Y counted from the top of the scrollback, and restores
// it clamped to the screen's top.
function saved(b) {
  return { x: b.savedX, y: Math.max(b.savedY - b.ybase, 0), fg: b.savedCurAttrData?.fg, bg: b.savedCurAttrData?.bg };
}

function snapshot(t) {
  const core = t._core;
  const active = t.buffer.active.type;
  return {
    active,
    normal: lines(t, t.buffer.normal),
    alternate: active === 'alternate' ? lines(t, t.buffer.alternate) : null,
    cursor: { x: t.buffer.active.cursorX, y: t.buffer.active.cursorY, rawX: core.buffer.x },
    savedNormal: saved(core.buffers.normal),
    savedAlternate: active === 'alternate' ? saved(core.buffers.alt) : null,
    modes: { ...t.modes },
    region: [core.buffer.scrollTop, core.buffer.scrollBottom],
    rows: t.rows,
  };
}

// ---------------------------------------------------------------------------
// Comparisons. Each returns a list of differences.

function compareLines(what, a, b, out) {
  if (a.length !== b.length) out.push(`${what}: ${a.length} lines != ${b.length}`);
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const la = a[a.length - n + i];
    const lb = b[b.length - n + i];
    if (la.wrapped !== lb.wrapped) out.push(`${what} line ${i}: wrapped ${la.wrapped} != ${lb.wrapped}`);
    for (let x = 0; x < la.cells.length; x++) {
      if (la.cells[x] !== lb.cells[x]) {
        out.push(`${what} line ${i} col ${x}: ${JSON.stringify(la.cells[x])} != ${JSON.stringify(lb.cells[x])} (${JSON.stringify(la.text)} / ${JSON.stringify(lb.text)})`);
        break;
      }
    }
  }
}

/// A against B, both xterm.js at the same size.
function compareXterm(A, B, { mode, history }) {
  const out = [];
  if (A.active !== B.active) out.push(`active buffer ${A.active} != ${B.active}`);
  let a = A.normal;
  let b = B.normal;
  if (mode === 'screen') {
    a = a.slice(a.length - A.rows);
    b = b.slice(b.length - B.rows);
  } else if (b.length < a.length && history !== undefined && b.length - B.rows <= history) {
    // History limited by the client's scrollback or the history cap: B
    // holds the newest part of A's.
    a = a.slice(a.length - b.length);
  }
  compareLines('normal', a, b, out);
  if (A.alternate) compareLines('alternate', A.alternate, B.alternate ?? [], out);
  for (const k of ['cursor', 'savedNormal', 'savedAlternate', 'modes', 'region']) {
    if (JSON.stringify(A[k]) !== JSON.stringify(B[k])) out.push(`${k}: ${JSON.stringify(A[k])} != ${JSON.stringify(B[k])}`);
  }
  return out;
}

/// B's rows against the model's own, by text: for history after a width
/// change.
function compareModel(expected, B, { mode }) {
  const out = [];
  const screens = expected.screens;
  // The model's rows drop trailing spaces; so does this comparison.
  const primary = screens.primary.rows.map((r) => r[0].trimEnd());
  let want = primary;
  let got = B.normal.map((l) => l.text.trimEnd());
  if (mode === 'screen') {
    want = want.slice(want.length - expected.rows);
    got = got.slice(got.length - B.rows);
  } else if (got.length < want.length) {
    want = want.slice(want.length - got.length);
  }
  if (want.length !== got.length) out.push(`normal: ${want.length} model rows != ${got.length} lines`);
  for (let i = 0; i < Math.min(want.length, got.length); i++) {
    if (want[i] !== got[i]) out.push(`normal row ${i}: model ${JSON.stringify(want[i])} != ${JSON.stringify(got[i])}`);
  }
  if (expected.active === 'alternate') {
    const alt = screens.alternate.rows.map((r) => r[0].trimEnd());
    const galt = (B.alternate ?? []).map((l) => l.text.trimEnd());
    for (let i = 0; i < Math.max(alt.length, galt.length); i++) {
      if (alt[i] !== galt[i]) out.push(`alternate row ${i}: model ${JSON.stringify(alt[i])} != ${JSON.stringify(galt[i])}`);
    }
  }
  const c = expected.screens[expected.active].cursor;
  if (c.x !== B.cursor.x || c.y !== B.cursor.y) out.push(`cursor: model ${c.x},${c.y} != ${B.cursor.x},${B.cursor.y}`);
  return out;
}

// ---------------------------------------------------------------------------
// A kitty keyboard stack with 4 entries per screen, ringed as Ghostty's 8.

class Kitty4 {
  constructor() {
    this.reset();
  }
  reset() {
    this.screens = { primary: { flags: [0, 0, 0, 0], idx: 0 }, alternate: { flags: [0, 0, 0, 0], idx: 0 } };
    this.active = 'primary';
  }
  get s() {
    return this.screens[this.active];
  }
  feed(bytes) {
    const text = Buffer.from(bytes).toString('latin1');
    const re = /\x1bc|\x1b\[([<>=?])([0-9;]*)([uhl])/g;
    for (const m of text.matchAll(re)) {
      if (m[0] === '\x1bc') {
        this.reset();
        continue;
      }
      const [, mark, params, fin] = m;
      const p = params.split(';').map((x) => (x === '' ? undefined : Number(x)));
      if (mark === '?' && (fin === 'h' || fin === 'l')) {
        for (const n of p) {
          if (n === 47 || n === 1047 || n === 1049) this.active = fin === 'h' ? 'alternate' : 'primary';
        }
      } else if (fin === 'u') {
        const s = this.s;
        if (mark === '>') {
          s.idx = (s.idx + 1) % 4;
          s.flags[s.idx] = p[0] ?? 0;
        } else if (mark === '<') {
          const n = p[0] ?? 1;
          if (n >= 4) {
            s.flags = [0, 0, 0, 0];
            s.idx = 0;
          } else {
            for (let i = 0; i < n; i++) {
              s.flags[s.idx] = 0;
              s.idx = (s.idx + 3) % 4;
            }
          }
        } else if (mark === '=') {
          const f = p[0] ?? 0;
          const how = p[1] ?? 1;
          s.flags[s.idx] = how === 1 ? f : how === 2 ? s.flags[s.idx] | f : s.flags[s.idx] & ~f;
        }
      }
    }
  }
  /// Oldest first, ending with the current entry.
  ring(screen) {
    const s = this.screens[screen];
    return [1, 2, 3, 4].map((i) => s.flags[(s.idx + i) % 4]);
  }
}

// ---------------------------------------------------------------------------
// Named assertions, on B (or A with `on: "A"`) and on the model's state.

function assertion(as, ctx) {
  const t = as.on === 'A' ? ctx.A : ctx.B;
  const snap = as.on === 'A' ? ctx.snapA : ctx.snapB;
  if ('cursor' in as) {
    const got = [snap.cursor.x, snap.cursor.y];
    return JSON.stringify(got) === JSON.stringify(as.cursor) ? null : `cursor ${JSON.stringify(got)}`;
  }
  if ('buffer' in as) return snap.active === as.buffer ? null : `buffer ${snap.active}`;
  if ('cursor_x' in as) return snap.cursor.x === as.cursor_x ? null : `cursor x ${snap.cursor.x}`;
  if ('contains' in as) {
    const buf = t.buffer.active;
    for (let y = 0; y < buf.length; y++) if (buf.getLine(y).translateToString(true).includes(as.contains)) return null;
    return `no line contains ${JSON.stringify(as.contains)}`;
  }
  if ('line' in as) {
    const [y, text] = as.line;
    const buf = t.buffer.active;
    const got = buf.getLine(buf.viewportY + y)?.translateToString(true);
    return got === text ? null : `line ${y} is ${JSON.stringify(got)}`;
  }
  if ('scrollback' in as) {
    const got = t.buffer.normal.length - t.rows;
    return got === as.scrollback ? null : `scrollback ${got}`;
  }
  if ('feeder' in as) {
    // What the supervisor's feeder did over the whole fixture.
    const got = { discarded: ctx.summary.discarded, fed_through: ctx.summary.fed_through };
    return JSON.stringify(got) === JSON.stringify(as.feeder) ? null : `feeder ${JSON.stringify(got)}`;
  }
  if ('scrollback_below' in as) {
    const got = t.buffer.normal.length - t.rows;
    return got > 0 && got < as.scrollback_below ? null : `scrollback ${got}`;
  }
  if ('model_mode' in as) {
    const [mode, value] = as.model_mode;
    return ctx.expected.modes[mode] === value ? null : `model mode ${mode} is ${ctx.expected.modes[mode]}`;
  }
  if ('kitty' in as) {
    const got = ctx.expected.screens[as.kitty.screen].kitty;
    return JSON.stringify(got) === JSON.stringify(as.kitty.flags) ? null : `model kitty ${JSON.stringify(got)}`;
  }
  if ('saved' in as) {
    const sc = ctx.expected.screens[as.saved.screen].saved_cursor;
    const got = sc ? [sc.x, sc.y] : null;
    return JSON.stringify(got) === JSON.stringify(as.saved.at) ? null : `model saved cursor ${JSON.stringify(got)}`;
  }
  if ('model_line' in as) {
    const [screen, y, text] = as.model_line;
    const s = ctx.expected.screens[screen];
    const rows = s.rows.slice(s.rows.length - ctx.expected.rows);
    return rows[y]?.[0] === text ? null : `model ${screen} row ${y} is ${JSON.stringify(rows[y]?.[0])}`;
  }
  return `unknown assertion ${JSON.stringify(as)}`;
}

// ---------------------------------------------------------------------------

async function runFixture(fx, dumped, version, report) {
  const make = load(version);
  const { script } = fx;
  const [cols0, rows0] = script.size;
  const A = make(cols0, rows0, script.scrollback ?? 1000);
  const kittyA = new Kitty4();
  let resized = false;
  let open = null; // the latest restore's window

  const fail = (where, list, known) => {
    for (const d of list) {
      const k = (known ?? []).find((k) => d.includes(k.match) && (!k.where || where.includes(k.where)));
      report.push({ fixture: fx.name, version, where, difference: d, known: k?.reason ?? null });
    }
  };

  const close = async () => {
    if (!open) return;
    const w = open;
    open = null;
    const r = dumped.summary.restores[w.index];
    if (r.partial) return;
    const live = dumped.file(`live-${w.index}.bin`);
    for (const [label, B, kitty] of w.clients) {
      await write(B, live);
      kitty?.feed(live);
      const snapB = snapshot(B);
      if (w.oracle === 'xterm') {
        fail(`restore ${w.index} after (${label})`, compareXterm(snapshot(A), snapB, w), script.known);
      } else {
        fail(`restore ${w.index} after (${label})`, compareModel(r.after, snapB, w), script.known);
      }
      if (kitty) {
        // The alternate screen's stack only while it is in use: a restore
        // writes the stack of each screen the program is using.
        for (const screen of r.after.active === 'alternate' ? ['primary', 'alternate'] : ['primary']) {
          const want = kittyA.ring(screen);
          const got = kitty.ring(screen);
          if (JSON.stringify(want) !== JSON.stringify(got)) {
            fail(`restore ${w.index} after (kitty4)`, [`${screen} 4-entry stack ${JSON.stringify(got)} != ${JSON.stringify(want)}`], script.known);
          }
        }
      }
      if (label === 'plain') {
        for (const as of w.restore.assert ?? []) {
          if (!as.after) continue;
          const err = assertion(as, { A, B, snapA: snapshot(A), snapB, expected: r.after, summary: dumped.summary });
          if (err) fail(`restore ${w.index} after: ${as.name}`, [err], script.known);
          else report.push({ fixture: fx.name, version, passed: as.name });
        }
      }
    }
  };

  for (const step of steps(fx)) {
    if (step.bytes) {
      await write(A, step.bytes);
      kittyA.feed(step.bytes);
    } else if (step.resize) {
      await close();
      A.resize(step.resize[0], step.resize[1]);
      resized = true;
    } else if (step.restore) {
      await close();
      const rs = step.restore;
      const r = dumped.summary.restores[step.index];
      const bytes = dumped.file(`restore-${step.index}.bin`);
      const [c, rw] = r.size;
      const where = `restore ${step.index}`;
      const sameSize = c === A.cols && rw === A.rows;
      const oracle = r.partial ? 'fallback' : rs.oracle ?? (sameSize && !resized ? 'xterm' : 'model');
      const ctx = { mode: r.mode, history: r.scrollback, oracle, index: step.index, restore: rs, clients: [] };

      // The perch-vt oracle, from model-dump.
      fail(`${where} (perch-vt)`, r.differences ?? [], script.known);
      fail(`${where} (perch-vt, hostile)`, r.differences_hostile ?? [], script.known);
      fail(`${where} after (perch-vt)`, r.differences_after ?? [], script.known);
      fail(`${where} after (perch-vt, hostile)`, r.differences_after_hostile ?? [], script.known);

      if (oracle === 'fallback') {
        const notice = Buffer.from(r.notice, 'utf8');
        const rest = bytes.subarray(r.prologue_len);
        const errs = [];
        if (!rest.subarray(0, notice.length).equals(notice)) errs.push('no notice after the prologue');
        let tail = rest.subarray(notice.length);
        const paste = Buffer.from('\x1b[?2004h');
        if (tail.subarray(0, paste.length).equals(paste)) tail = tail.subarray(paste.length);
        if (tail.length !== r.held) errs.push(`${tail.length - r.held} bytes after the notice`);
        if (!r.nudge) errs.push('no resize nudge');
        fail(`${where} (fallback)`, errs, script.known);
        if (!errs.length) report.push({ fixture: fx.name, version, passed: `${where} is prologue, notice and nudge` });
        open = { ...ctx, clients: [] };
        continue;
      }

      const hostile = dumped.file('hostile.bin');
      const clients = [];
      for (const label of ['plain', 'hostile']) {
        const B = make(c, rw, r.scrollback);
        let kitty = null;
        if (label === 'hostile') await write(B, hostile);
        let before = null;
        if (r.mode === 'screen' && label === 'plain') {
          // A screen restore keeps what the terminal already had.
          await write(B, Buffer.from(Array.from({ length: rw + 3 }, (_, i) => `earlier ${i}`).join('\r\n')));
          before = B.buffer.normal.length - B.rows;
        }
        if (label === 'plain' && rs.kitty4) {
          kitty = new Kitty4();
          kitty.feed(bytes);
        }
        await write(B, bytes);
        const snapB = snapshot(B);
        let errs;
        if (oracle === 'xterm') errs = compareXterm(snapshot(A), snapB, ctx);
        else errs = compareModel(r.expected, snapB, ctx);
        if (before !== null) {
          const kept = B.buffer.normal.getLine(0)?.translateToString(true);
          if (kept !== 'earlier 0') errs.push(`screen restore lost the scrollback (first line ${JSON.stringify(kept)})`);
        }
        fail(`${where} (${oracle}, ${label})`, errs, script.known);
        if (kitty) {
          for (const screen of r.expected.active === 'alternate' ? ['primary', 'alternate'] : ['primary']) {
            const want = r.expected.screens[screen].kitty.slice(4);
            const got = kitty.ring(screen);
            if (JSON.stringify(want) !== JSON.stringify(got)) {
              fail(`${where} (kitty4)`, [`${screen} 4-entry stack ${JSON.stringify(got)} != newest of ${JSON.stringify(want)}`], script.known);
            }
          }
        }
        if (label === 'plain') {
          for (const as of rs.assert ?? []) {
            if (as.after) continue;
            const err = assertion(as, { A, B, snapA: snapshot(A), snapB, expected: r.expected, summary: dumped.summary });
            if (err) fail(`${where}: ${as.name}`, [err], script.known);
            else report.push({ fixture: fx.name, version, passed: as.name });
          }
        }
        clients.push([label, B, kitty]);
      }
      open = { ...ctx, clients };
    }
  }
  await close();
}

async function main() {
  const opts = options(process.argv.slice(2));
  const versions = opts.xterm.length ? opts.xterm : VERSIONS;
  const report = [];
  let failed = 0;
  for (const fx of fixtures(opts.only)) {
    const dumped = modelDump(opts, fx);
    for (const version of versions) {
      const start = report.length;
      await runFixture(fx, dumped, version, report);
      const mine = report.slice(start);
      const bad = mine.filter((r) => r.difference && !r.known);
      const known = mine.filter((r) => r.difference && r.known);
      const passed = mine.filter((r) => r.passed);
      failed += bad.length;
      console.log(`${bad.length ? 'FAIL' : 'ok  '} ${fx.name} xterm ${version}: ${passed.length} named assertions, ${known.length} known differences${bad.length ? `, ${bad.length} failures` : ''}`);
      for (const r of bad.slice(0, 12)) console.log(`       ${r.where}: ${r.difference.slice(0, 400)}`);
      if (bad.length > 12) console.log(`       ... ${bad.length - 12} more`);
    }
    if (!opts.keep) rmSync(dumped.out, { recursive: true, force: true });
  }
  const known = new Map();
  for (const r of report) if (r.known) known.set(`${r.fixture}: ${r.known}`, (known.get(`${r.fixture}: ${r.known}`) ?? 0) + 1);
  for (const [k, n] of known) console.log(`known: ${k} (${n})`);
  if (failed) {
    console.log(`fidelity: ${failed} failures`);
    process.exit(1);
  }
  console.log(`fidelity: ok (${here})`);
}

await main();
