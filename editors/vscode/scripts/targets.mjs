// SPDX-License-Identifier: GPL-3.0-only

// The VSIXs Perch ships: one per VS Code target platform, and a universal
// one without a binary. `dir` is the `<os>-<arch>` directory under bin/ that
// src/core/binary.ts looks in. Node calls Alpine `linux`, so the alpine
// targets carry the same static musl binary under the same directory as the
// linux ones.
export const targets = {
  'darwin-x64': { dir: 'darwin-x64', format: 'macho', arch: 'x64' },
  'darwin-arm64': { dir: 'darwin-arm64', format: 'macho', arch: 'arm64' },
  'linux-x64': { dir: 'linux-x64', format: 'elf', arch: 'x64' },
  'linux-arm64': { dir: 'linux-arm64', format: 'elf', arch: 'arm64' },
  'alpine-x64': { dir: 'linux-x64', format: 'elf', arch: 'x64' },
  'alpine-arm64': { dir: 'linux-arm64', format: 'elf', arch: 'arm64' },
};

export const universal = 'universal';

/** The VSIX file name for a target (or `universal`) and version. */
export function vsixName(target, version) {
  return `perch-${target}-${version}.vsix`;
}

/** The target, or `universal`, or an error naming the valid ones. */
export function checkTarget(target) {
  if (target === universal || Object.hasOwn(targets, target)) return target;
  throw new Error(`unknown target \`${target}\`; one of ${[...Object.keys(targets), universal].join(', ')}`);
}
