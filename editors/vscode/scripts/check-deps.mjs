// SPDX-License-Identifier: GPL-3.0-only

// Fails unless package.json has no runtime dependencies and every package in
// package-lock.json has an allowed license and is listed in docs/licenses.md.
// With --write, regenerates the table in docs/licenses.md instead.
import { readFileSync, writeFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const docPath = join(root, '../../docs/licenses.md');
const allowed = new Set([
  'MIT',
  'Apache-2.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'ISC',
  'Zlib',
  'BlueOak-1.0.0',
  'Python-2.0',
]);

const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
const errors = [];

if (Object.keys(pkg.dependencies ?? {}).length > 0) {
  errors.push('package.json: `dependencies` must be empty; the extension ships no npm packages');
}

const rows = [];
for (const [where, info] of Object.entries(lock.packages)) {
  if (where === '') continue;
  if (!info.dev) errors.push(`${where}: not a development dependency`);
  const name = where.slice(where.lastIndexOf('node_modules/') + 'node_modules/'.length);
  const license = info.license ?? 'UNKNOWN';
  if (!licenseAllowed(license)) errors.push(`${name}@${info.version}: license ${license} is not allowed`);
  rows.push(`| ${name} | ${info.version} | ${license} |`);
}
rows.sort();
const unique = [...new Set(rows)];

const begin = '<!-- begin npm packages -->';
const end = '<!-- end npm packages -->';
const table = [begin, '| Package | Version | License |', '|---------|---------|---------|', ...unique, end].join('\n');
const doc = readFileSync(docPath, 'utf8');
const current = doc.slice(doc.indexOf(begin), doc.indexOf(end) + end.length);

if (process.argv.includes('--write')) {
  writeFileSync(docPath, doc.replace(current, table));
  console.log(`docs/licenses.md: ${unique.length} packages`);
} else {
  if (current !== table) errors.push('docs/licenses.md is out of date; run `npm run licenses`');
  for (const e of errors) console.error(e);
  if (errors.length > 0) process.exit(1);
  console.log(`check-deps: ok (${unique.length} development packages, no runtime dependencies)`);
}

// An SPDX expression is allowed if every alternative of an OR, or every term
// of an AND, is.
function licenseAllowed(expr) {
  const e = expr.replace(/^\((.*)\)$/, '$1').trim();
  if (e.includes(' OR ')) return e.split(' OR ').some((x) => licenseAllowed(x));
  if (e.includes(' AND ')) return e.split(' AND ').every((x) => licenseAllowed(x));
  return allowed.has(e);
}
