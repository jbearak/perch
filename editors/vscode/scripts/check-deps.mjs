// SPDX-License-Identifier: GPL-3.0-only

// Fails unless package.json has no runtime dependencies and every package in
// package-lock.json has an allowed license and is listed in docs/licenses.md.
// The same for release/, the packaging and publishing tools, which run only
// in CI and allow more licenses (none of them is shipped). With --write,
// regenerates the tables in docs/licenses.md instead.
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
// docs/licenses.md says why each of these is acceptable for a tool that is
// run and not shipped.
const releaseAllowed = new Set([...allowed, 'Artistic-2.0', '0BSD', 'EPL-2.0']);
const releaseExceptions = new Map([[/^@vscode\/vsce-sign(-[a-z0-9-]+)?$/, 'SEE LICENSE IN LICENSE.txt']]);

const sets = [
  { dir: root, marker: 'npm packages', allowed, exceptions: new Map() },
  { dir: join(root, 'release'), marker: 'release tools', allowed: releaseAllowed, exceptions: releaseExceptions },
];

const errors = [];
let doc = readFileSync(docPath, 'utf8');
const counts = [];
for (const set of sets) {
  const { table, count } = check(set);
  const begin = `<!-- begin ${set.marker} -->`;
  const end = `<!-- end ${set.marker} -->`;
  const current = doc.slice(doc.indexOf(begin), doc.indexOf(end) + end.length);
  const wanted = [begin, '| Package | Version | License |', '|---------|---------|---------|', ...table, end].join('\n');
  if (!doc.includes(begin) || !doc.includes(end)) errors.push(`docs/licenses.md has no ${begin} … ${end} section`);
  else if (process.argv.includes('--write')) doc = doc.replace(current, wanted);
  else if (current !== wanted) errors.push('docs/licenses.md is out of date; run `npm run licenses`');
  counts.push(`${count} ${set.marker}`);
}

if (process.argv.includes('--write')) {
  writeFileSync(docPath, doc);
  console.log(`docs/licenses.md: ${counts.join(', ')}`);
} else {
  for (const e of errors) console.error(e);
  if (errors.length > 0) process.exit(1);
  console.log(`check-deps: ok (${counts.join(', ')}, no runtime dependencies)`);
}

function check({ dir, allowed, exceptions }) {
  const pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const lock = JSON.parse(readFileSync(join(dir, 'package-lock.json'), 'utf8'));
  if (Object.keys(pkg.dependencies ?? {}).length > 0) {
    errors.push(`${pkg.name}: \`dependencies\` must be empty; the extension ships no npm packages`);
  }
  const rows = [];
  for (const [where, info] of Object.entries(lock.packages)) {
    if (where === '') continue;
    if (!info.dev) errors.push(`${where}: not a development dependency`);
    const name = where.slice(where.lastIndexOf('node_modules/') + 'node_modules/'.length);
    const license = info.license ?? 'UNKNOWN';
    const excepted = [...exceptions].some(([re, l]) => re.test(name) && l === license);
    if (!excepted && !licenseAllowed(license, allowed)) {
      errors.push(`${name}@${info.version}: license ${license} is not allowed`);
    }
    rows.push(`| ${name} | ${info.version} | ${license} |`);
  }
  const table = [...new Set(rows.sort())];
  return { table, count: table.length };
}

// An SPDX expression is allowed if every alternative of an OR, or every term
// of an AND, is.
function licenseAllowed(expr, allowed) {
  const e = expr.replace(/^\((.*)\)$/, '$1').trim();
  if (e.includes(' OR ')) return e.split(' OR ').some((x) => licenseAllowed(x, allowed));
  if (e.includes(' AND ')) return e.split(' AND ').every((x) => licenseAllowed(x, allowed));
  return allowed.has(e);
}
