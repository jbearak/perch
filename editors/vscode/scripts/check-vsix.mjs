// SPDX-License-Identifier: GPL-3.0-only

// Checks a packaged VSIX: its target platform and version, that it holds
// only what the extension needs plus LICENSE and NOTICE, and that its binary
// is where src/core/binary.ts looks, for the right OS and architecture, static
// (ELF: no interpreter) or ad-hoc signed (Mach-O: a code signature whose code
// directory has the ad-hoc flag and not the linker-signed one). A universal
// VSIX has no binary. Reads the VSIX with `unzip`.
//
//   node scripts/check-vsix.mjs perch-linux-x64-0.1.0.vsix linux-x64
import { execFileSync } from 'child_process';
import { readFileSync } from 'fs';
import { basename, dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { checkTarget, targets, universal } from './targets.mjs';

const allowed = [
  /^\[Content_Types\]\.xml$/,
  /^extension\.vsixmanifest$/,
  /^extension\/package\.json$/,
  /^extension\/dist\/extension\.js$/,
  /^extension\/media\/[^/]+$/,
  /^extension\/LICENSE(\.txt)?$/,
  /^extension\/NOTICE(\.txt)?$/,
  /^extension\/readme\.md$/i,
  /^extension\/bin\/[^/]+\/perch$/,
];
const required = [/^extension\/dist\/extension\.js$/, /^extension\/LICENSE(\.txt)?$/, /^extension\/NOTICE(\.txt)?$/];

/** Throws, naming every problem, unless the VSIX is right for the target. */
export function checkVsix(vsix, target, version) {
  checkTarget(target);
  const errors = [];
  const entries = execFileSync('unzip', ['-Z1', vsix], { encoding: 'utf8' }).split('\n').filter((e) => e !== '');
  const read = (name) => execFileSync('unzip', ['-p', vsix, name], { maxBuffer: 1 << 28 });

  for (const e of entries) {
    if (!e.endsWith('/') && !allowed.some((re) => re.test(e))) errors.push(`unexpected file ${e}`);
  }
  for (const re of required) {
    if (!entries.some((e) => re.test(e))) errors.push(`no file matching ${re}`);
  }

  const manifest = read('extension.vsixmanifest').toString('utf8');
  const platform = /<Identity\b[^>]*\bTargetPlatform="([^"]*)"/.exec(manifest)?.[1];
  const expectedPlatform = target === universal ? undefined : target;
  if (platform !== expectedPlatform) {
    errors.push(`manifest target platform ${platform ?? '(none)'}, expected ${expectedPlatform ?? '(none)'}`);
  }
  const pkg = JSON.parse(read('extension/package.json').toString('utf8'));
  if (pkg.version !== version) errors.push(`version ${pkg.version}, expected ${version}`);

  const binaries = entries.filter((e) => e.startsWith('extension/bin/') && !e.endsWith('/'));
  if (target === universal) {
    if (binaries.length > 0) errors.push(`the universal VSIX has binaries: ${binaries.join(', ')}`);
  } else {
    const want = `extension/bin/${targets[target].dir}/perch`;
    if (binaries.length !== 1 || binaries[0] !== want) {
      errors.push(`binaries ${JSON.stringify(binaries)}, expected exactly ${want}`);
    } else {
      errors.push(...checkBinary(read(want), targets[target]));
      // zipinfo's line starts with the Unix mode, such as -rwxr-xr-x.
      const mode = execFileSync('unzip', ['-Z', vsix, want], { encoding: 'utf8' }).slice(0, 10);
      if (mode !== '-rwxr-xr-x') errors.push(`${want} has mode ${mode}, expected -rwxr-xr-x`);
    }
  }

  if (errors.length > 0) throw new Error(`${basename(vsix)}:\n  ${errors.join('\n  ')}`);
  console.log(`check-vsix: ok (${basename(vsix)}: ${target}, ${version}, ${entries.length} entries)`);
}

/** Problems with a binary's format and architecture, or none. */
export function checkBinary(buf, { format, arch }) {
  return format === 'elf' ? checkElf(buf, arch) : checkMachO(buf, arch);
}

function checkElf(buf, arch) {
  if (buf.length < 64 || buf.readUInt32BE(0) !== 0x7f454c46) return ['binary is not ELF'];
  if (buf[4] !== 2 || buf[5] !== 1) return ['binary is not 64-bit little-endian ELF'];
  const machine = { 62: 'x64', 183: 'arm64' }[buf.readUInt16LE(18)] ?? `e_machine ${buf.readUInt16LE(18)}`;
  const errors = machine === arch ? [] : [`binary is ${machine}, expected ${arch}`];
  const phoff = Number(buf.readBigUInt64LE(32));
  const phentsize = buf.readUInt16LE(54);
  const phnum = buf.readUInt16LE(56);
  for (let i = 0; i < phnum; i++) {
    // PT_INTERP: a dynamic loader, so not a static build.
    if (buf.readUInt32LE(phoff + i * phentsize) === 3) errors.push('binary has an interpreter (not static)');
  }
  return errors;
}

function checkMachO(buf, arch) {
  if (buf.length < 32 || buf.readUInt32LE(0) !== 0xfeedfacf) return ['binary is not 64-bit Mach-O'];
  const cpu = buf.readUInt32LE(4);
  const machine = { 0x01000007: 'x64', 0x0100000c: 'arm64' }[cpu] ?? `cputype 0x${cpu.toString(16)}`;
  const errors = machine === arch ? [] : [`binary is ${machine}, expected ${arch}`];
  const ncmds = buf.readUInt32LE(16);
  let at = 32;
  let signature;
  for (let i = 0; i < ncmds; i++) {
    const cmd = buf.readUInt32LE(at);
    // LC_CODE_SIGNATURE
    if (cmd === 0x1d) signature = { offset: buf.readUInt32LE(at + 8), size: buf.readUInt32LE(at + 12) };
    at += buf.readUInt32LE(at + 4);
  }
  if (signature === undefined) return [...errors, 'binary has no code signature'];
  // A SuperBlob (big-endian) indexing blobs; slot 0 is the code directory.
  const sb = signature.offset;
  if (buf.readUInt32BE(sb) !== 0xfade0cc0) return [...errors, 'code signature is not a SuperBlob'];
  const count = buf.readUInt32BE(sb + 8);
  for (let i = 0; i < count; i++) {
    if (buf.readUInt32BE(sb + 12 + i * 8) !== 0) continue;
    const cd = sb + buf.readUInt32BE(sb + 16 + i * 8);
    if (buf.readUInt32BE(cd) !== 0xfade0c02) return [...errors, 'slot 0 is not a code directory'];
    const flags = buf.readUInt32BE(cd + 12);
    // CS_ADHOC 0x2; CS_LINKER_SIGNED 0x20000 marks the linker's own
    // signature, which `codesign -s -` replaces.
    if ((flags & 0x2) === 0) errors.push('code signature is not ad-hoc');
    if ((flags & 0x20000) !== 0) errors.push('code signature is the linker\'s, not `codesign -s -`');
    return errors;
  }
  return [...errors, 'code signature has no code directory'];
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [vsix, target] = process.argv.slice(2);
  if (vsix === undefined || target === undefined) {
    console.error('usage: check-vsix.mjs <vsix> <target>');
    process.exit(2);
  }
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;
  try {
    checkVsix(vsix, target, version);
  } catch (err) {
    console.error(`check-vsix: ${err.message}`);
    process.exit(1);
  }
}
