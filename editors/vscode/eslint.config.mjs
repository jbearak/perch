// SPDX-License-Identifier: GPL-3.0-only

import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist/', 'out/', '.vscode-test/', 'node_modules/'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    // Build and check scripts run in Node.
    files: ['*.mjs', 'scripts/**/*.mjs'],
    languageOptions: { globals: { process: 'readonly', console: 'readonly' } },
  },
  {
    // src/core/ holds plain logic, tested without VS Code.
    files: ['src/core/**/*.ts'],
    rules: {
      'no-restricted-imports': ['error', { paths: ['vscode'] }],
    },
  },
);
