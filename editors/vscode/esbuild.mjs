// SPDX-License-Identifier: GPL-3.0-only

// Bundles the extension into dist/extension.js for the Node 18 that VS Code
// 1.82 ships.
import { build } from 'esbuild';

await build({
  entryPoints: ['src/extension.ts'],
  outfile: 'dist/extension.js',
  bundle: true,
  platform: 'node',
  target: 'node18',
  format: 'cjs',
  external: ['vscode'],
  sourcemap: true,
  logLevel: 'info',
});
