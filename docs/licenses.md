# Licenses of the extension's npm packages

The VS Code extension ships no npm packages: `dependencies` in `editors/vscode/package.json` is empty, and CI fails if it is not. The packages below are used only to build, lint and test it. They are pinned by `editors/vscode/package-lock.json`, and `npm run check-deps` checks in CI that this table matches the lockfile and that every license is on the allow-list in `editors/vscode/scripts/check-deps.mjs`. Regenerate the table with `npm run licenses`.

`package.json` overrides `serialize-javascript` to 7.1.4 to clear advisory GHSA-5c6j-r48x-rmvq in the version mocha 10 pins. Mocha loads it only for parallel runs, which the tests do not use. Mocha stays on 10 because the tests run inside VS Code 1.82's Node 18.

Rust crates are checked by `cargo deny` (see `deny.toml`), and the code linked into the `perch` binary is credited in `NOTICE`.

<!-- begin npm packages -->
| Package | Version | License |
|---------|---------|---------|
| @cacheable/memory | 2.2.0 | MIT |
| @cacheable/utils | 2.5.0 | MIT |
| @esbuild/aix-ppc64 | 0.28.2 | MIT |
| @esbuild/android-arm | 0.28.2 | MIT |
| @esbuild/android-arm64 | 0.28.2 | MIT |
| @esbuild/android-x64 | 0.28.2 | MIT |
| @esbuild/darwin-arm64 | 0.28.2 | MIT |
| @esbuild/darwin-x64 | 0.28.2 | MIT |
| @esbuild/freebsd-arm64 | 0.28.2 | MIT |
| @esbuild/freebsd-x64 | 0.28.2 | MIT |
| @esbuild/linux-arm | 0.28.2 | MIT |
| @esbuild/linux-arm64 | 0.28.2 | MIT |
| @esbuild/linux-ia32 | 0.28.2 | MIT |
| @esbuild/linux-loong64 | 0.28.2 | MIT |
| @esbuild/linux-mips64el | 0.28.2 | MIT |
| @esbuild/linux-ppc64 | 0.28.2 | MIT |
| @esbuild/linux-riscv64 | 0.28.2 | MIT |
| @esbuild/linux-s390x | 0.28.2 | MIT |
| @esbuild/linux-x64 | 0.28.2 | MIT |
| @esbuild/netbsd-arm64 | 0.28.2 | MIT |
| @esbuild/netbsd-x64 | 0.28.2 | MIT |
| @esbuild/openbsd-arm64 | 0.28.2 | MIT |
| @esbuild/openbsd-x64 | 0.28.2 | MIT |
| @esbuild/openharmony-arm64 | 0.28.2 | MIT |
| @esbuild/sunos-x64 | 0.28.2 | MIT |
| @esbuild/win32-arm64 | 0.28.2 | MIT |
| @esbuild/win32-ia32 | 0.28.2 | MIT |
| @esbuild/win32-x64 | 0.28.2 | MIT |
| @eslint-community/eslint-utils | 4.10.1 | MIT |
| @eslint-community/regexpp | 4.12.2 | MIT |
| @eslint/config-array | 0.23.5 | Apache-2.0 |
| @eslint/config-helpers | 0.7.0 | Apache-2.0 |
| @eslint/core | 1.2.1 | Apache-2.0 |
| @eslint/js | 10.0.1 | MIT |
| @eslint/object-schema | 3.0.5 | Apache-2.0 |
| @eslint/plugin-kit | 0.7.3 | Apache-2.0 |
| @humanfs/core | 0.19.2 | Apache-2.0 |
| @humanfs/node | 0.16.8 | Apache-2.0 |
| @humanfs/types | 0.15.0 | Apache-2.0 |
| @humanwhocodes/module-importer | 1.0.1 | Apache-2.0 |
| @humanwhocodes/retry | 0.4.3 | Apache-2.0 |
| @keyv/bigmap | 1.3.1 | MIT |
| @keyv/serialize | 1.1.1 | MIT |
| @types/esrecurse | 4.3.1 | MIT |
| @types/estree | 1.0.10 | MIT |
| @types/json-schema | 7.0.15 | MIT |
| @types/mocha | 10.0.10 | MIT |
| @types/node | 18.19.130 | MIT |
| @types/vscode | 1.82.0 | MIT |
| @typescript-eslint/eslint-plugin | 8.72.1 | MIT |
| @typescript-eslint/parser | 8.72.1 | MIT |
| @typescript-eslint/project-service | 8.72.1 | MIT |
| @typescript-eslint/scope-manager | 8.72.1 | MIT |
| @typescript-eslint/tsconfig-utils | 8.72.1 | MIT |
| @typescript-eslint/type-utils | 8.72.1 | MIT |
| @typescript-eslint/types | 8.72.1 | MIT |
| @typescript-eslint/typescript-estree | 8.72.1 | MIT |
| @typescript-eslint/utils | 8.72.1 | MIT |
| @typescript-eslint/visitor-keys | 8.72.1 | MIT |
| @vscode/test-electron | 3.1.0 | MIT |
| acorn | 8.19.0 | MIT |
| acorn-jsx | 5.3.2 | MIT |
| agent-base | 7.1.4 | MIT |
| ajv | 6.15.0 | MIT |
| ansi-colors | 4.1.3 | MIT |
| ansi-regex | 5.0.1 | MIT |
| ansi-regex | 6.5.0 | MIT |
| ansi-styles | 4.3.0 | MIT |
| anymatch | 3.1.3 | ISC |
| argparse | 2.0.1 | Python-2.0 |
| balanced-match | 1.0.2 | MIT |
| balanced-match | 4.0.4 | MIT |
| binary-extensions | 2.3.0 | MIT |
| brace-expansion | 2.1.7 | MIT |
| brace-expansion | 5.0.12 | MIT |
| braces | 3.0.3 | MIT |
| browser-stdout | 1.3.1 | ISC |
| cacheable | 2.5.0 | MIT |
| camelcase | 6.3.0 | MIT |
| chalk | 4.1.2 | MIT |
| chalk | 5.6.2 | MIT |
| chokidar | 3.6.0 | MIT |
| cli-cursor | 5.0.0 | MIT |
| cli-spinners | 2.9.2 | MIT |
| cliui | 7.0.4 | ISC |
| color-convert | 2.0.1 | MIT |
| color-name | 1.1.4 | MIT |
| core-util-is | 1.0.3 | MIT |
| cross-spawn | 7.0.6 | MIT |
| debug | 4.4.3 | MIT |
| decamelize | 4.0.0 | MIT |
| deep-is | 0.1.4 | MIT |
| diff | 5.2.2 | BSD-3-Clause |
| emoji-regex | 10.6.0 | MIT |
| emoji-regex | 8.0.0 | MIT |
| esbuild | 0.28.2 | MIT |
| escalade | 3.2.0 | MIT |
| escape-string-regexp | 4.0.0 | MIT |
| eslint | 10.13.0 | MIT |
| eslint-scope | 9.1.2 | BSD-2-Clause |
| eslint-visitor-keys | 3.4.3 | Apache-2.0 |
| eslint-visitor-keys | 5.0.1 | Apache-2.0 |
| espree | 11.2.0 | BSD-2-Clause |
| esquery | 1.7.0 | BSD-3-Clause |
| esrecurse | 4.3.0 | BSD-2-Clause |
| estraverse | 5.3.0 | BSD-2-Clause |
| esutils | 2.0.3 | BSD-2-Clause |
| fast-deep-equal | 3.1.3 | MIT |
| fast-json-stable-stringify | 2.1.0 | MIT |
| fast-levenshtein | 2.0.6 | MIT |
| fdir | 6.5.0 | MIT |
| file-entry-cache | 11.1.5 | MIT |
| fill-range | 7.1.1 | MIT |
| find-up | 5.0.0 | MIT |
| flat | 5.0.2 | BSD-3-Clause |
| flat-cache | 6.1.23 | MIT |
| flatted | 3.4.4 | ISC |
| fs.realpath | 1.0.0 | ISC |
| fsevents | 2.3.3 | MIT |
| get-caller-file | 2.0.5 | ISC |
| get-east-asian-width | 1.7.0 | MIT |
| glob | 8.1.0 | ISC |
| glob-parent | 5.1.2 | ISC |
| glob-parent | 6.0.2 | ISC |
| has-flag | 4.0.0 | MIT |
| hashery | 1.5.1 | MIT |
| he | 1.2.0 | MIT |
| hookified | 1.15.1 | MIT |
| hookified | 2.2.0 | MIT |
| http-proxy-agent | 7.0.2 | MIT |
| https-proxy-agent | 7.0.6 | MIT |
| ignore | 5.3.2 | MIT |
| ignore | 7.0.14 | MIT |
| immediate | 3.0.6 | MIT |
| imurmurhash | 0.1.4 | MIT |
| inflight | 1.0.6 | ISC |
| inherits | 2.0.4 | ISC |
| is-binary-path | 2.1.0 | MIT |
| is-extglob | 2.1.1 | MIT |
| is-fullwidth-code-point | 3.0.0 | MIT |
| is-glob | 4.0.3 | MIT |
| is-interactive | 2.0.0 | MIT |
| is-number | 7.0.0 | MIT |
| is-plain-obj | 2.1.0 | MIT |
| is-unicode-supported | 0.1.0 | MIT |
| is-unicode-supported | 1.3.0 | MIT |
| is-unicode-supported | 2.1.0 | MIT |
| isarray | 1.0.0 | MIT |
| isexe | 2.0.0 | ISC |
| js-yaml | 4.3.2 | MIT |
| json-schema-traverse | 0.4.1 | MIT |
| json-stable-stringify-without-jsonify | 1.0.1 | MIT |
| jszip | 3.10.2 | (MIT OR GPL-3.0-or-later) |
| keyv | 5.6.0 | MIT |
| levn | 0.4.1 | MIT |
| lie | 3.3.0 | MIT |
| locate-path | 6.0.0 | MIT |
| log-symbols | 4.1.0 | MIT |
| log-symbols | 6.0.0 | MIT |
| mimic-function | 5.0.1 | MIT |
| minimatch | 10.2.6 | BlueOak-1.0.0 |
| minimatch | 5.1.9 | ISC |
| mocha | 10.8.2 | MIT |
| ms | 2.1.3 | MIT |
| natural-compare | 1.4.0 | MIT |
| normalize-path | 3.0.0 | MIT |
| once | 1.4.0 | ISC |
| onetime | 7.0.0 | MIT |
| optionator | 0.9.4 | MIT |
| ora | 8.2.0 | MIT |
| p-limit | 3.1.0 | MIT |
| p-locate | 5.0.0 | MIT |
| pako | 1.0.11 | (MIT AND Zlib) |
| path-exists | 4.0.0 | MIT |
| path-key | 3.1.1 | MIT |
| picomatch | 2.3.2 | MIT |
| picomatch | 4.0.7 | MIT |
| prelude-ls | 1.2.1 | MIT |
| process-nextick-args | 2.0.1 | MIT |
| punycode | 2.3.1 | MIT |
| qified | 0.10.1 | MIT |
| readable-stream | 2.3.8 | MIT |
| readdirp | 3.6.0 | MIT |
| require-directory | 2.1.1 | MIT |
| restore-cursor | 5.1.0 | MIT |
| safe-buffer | 5.1.2 | MIT |
| semver | 7.8.5 | ISC |
| serialize-javascript | 7.1.4 | BSD-3-Clause |
| setimmediate | 1.0.5 | MIT |
| shebang-command | 2.0.0 | MIT |
| shebang-regex | 3.0.0 | MIT |
| signal-exit | 4.1.0 | ISC |
| stdin-discarder | 0.2.2 | MIT |
| string-width | 4.2.3 | MIT |
| string-width | 7.2.0 | MIT |
| string_decoder | 1.1.1 | MIT |
| strip-ansi | 6.0.1 | MIT |
| strip-ansi | 7.2.0 | MIT |
| strip-json-comments | 3.1.1 | MIT |
| supports-color | 7.2.0 | MIT |
| supports-color | 8.1.1 | MIT |
| tinyglobby | 0.2.17 | MIT |
| to-regex-range | 5.0.1 | MIT |
| ts-api-utils | 2.5.0 | MIT |
| type-check | 0.4.0 | MIT |
| typescript | 6.0.3 | Apache-2.0 |
| typescript-eslint | 8.72.1 | MIT |
| undici-types | 5.26.5 | MIT |
| uri-js | 4.4.1 | BSD-2-Clause |
| util-deprecate | 1.0.2 | MIT |
| which | 2.0.2 | ISC |
| word-wrap | 1.2.5 | MIT |
| workerpool | 6.5.1 | Apache-2.0 |
| wrap-ansi | 7.0.0 | MIT |
| wrappy | 1.0.2 | ISC |
| y18n | 5.0.8 | ISC |
| yargs | 16.2.2 | MIT |
| yargs-parser | 20.2.9 | ISC |
| yargs-unparser | 2.0.0 | MIT |
| yocto-queue | 0.1.0 | MIT |
<!-- end npm packages -->

## Restore fidelity harness

`tests/fidelity` runs restores into three versions of the headless xterm.js, each with its Unicode 11 addon, installed side by side under npm aliases. They are test-only and never shipped. They are pinned by `tests/fidelity/package-lock.json`, and `npm run check-deps` in that directory checks in CI that this table matches the lockfile and that every license is on the same allow-list as the extension's. Regenerate the table with `npm run licenses` there.

<!-- begin fidelity packages -->
| Package | Version | License |
|---------|---------|---------|
| @xterm/addon-unicode11 (as xterm-unicode11-5.5) | 0.8.0 | MIT |
| @xterm/addon-unicode11 (as xterm-unicode11-6.0) | 0.9.0 | MIT |
| @xterm/headless (as xterm-headless-5.5) | 5.5.0 | MIT |
| @xterm/headless (as xterm-headless-6.0) | 6.0.0 | MIT |
| xterm-addon-unicode11 (as xterm-unicode11-5.3) | 0.6.0 | MIT |
| xterm-headless (as xterm-headless-5.3) | 5.3.0-beta.61 | MIT |
<!-- end fidelity packages -->
