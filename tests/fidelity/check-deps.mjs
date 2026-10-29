// SPDX-License-Identifier: GPL-3.0-only

// Fails unless every package in the harness lockfile is a development
// dependency with an allowed license and is listed in docs/licenses.md.
// With --write, regenerates that table instead. The allow-list is the
// extension's (editors/vscode/scripts/check-deps.mjs).

import { readFileSync, writeFileSync } from 'fs';
import { join } from 'path';
import { here, repo } from './lib.mjs';

const allowed = new Set(['MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib', 'BlueOak-1.0.0', 'Python-2.0']);
const docPath = join(repo, 'docs/licenses.md');
const lock = JSON.parse(readFileSync(join(here, 'package-lock.json'), 'utf8'));
const errors = [];
const rows = [];
for (const [where, info] of Object.entries(lock.packages)) {
  if (where === '') continue;
  if (!info.dev) errors.push(`${where}: not a development dependency`);
  const alias = where.slice(where.lastIndexOf('node_modules/') + 'node_modules/'.length);
  const name = info.name ? `${info.name} (as ${alias})` : alias;
  const license = info.license ?? 'UNKNOWN';
  if (!allowed.has(license)) errors.push(`${name}@${info.version}: license ${license} is not allowed`);
  rows.push(`| ${name} | ${info.version} | ${license} |`);
}
rows.sort();

const begin = '<!-- begin fidelity packages -->';
const end = '<!-- end fidelity packages -->';
const table = [begin, '| Package | Version | License |', '|---------|---------|---------|', ...rows, end].join('\n');
const doc = readFileSync(docPath, 'utf8');
const current = doc.slice(doc.indexOf(begin), doc.indexOf(end) + end.length);

if (process.argv.includes('--write')) {
  writeFileSync(docPath, doc.replace(current, table));
  console.log(`docs/licenses.md: ${rows.length} fidelity packages`);
} else {
  if (current !== table) errors.push('docs/licenses.md is out of date; run `npm run licenses` in tests/fidelity');
  for (const e of errors) console.error(e);
  if (errors.length > 0) process.exit(1);
  console.log(`check-deps: ok (${rows.length} test packages)`);
}
