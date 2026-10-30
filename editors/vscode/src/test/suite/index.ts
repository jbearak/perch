// SPDX-License-Identifier: GPL-3.0-only

// Mocha entry point that VS Code's extension host calls. With
// PERCH_TEST_PHASE set, only the cross-process restore runs, in that phase.
import * as fs from 'fs';
import * as path from 'path';
import Mocha from 'mocha';

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: 'bdd', color: true, timeout: 60_000 });
  const phased = (process.env.PERCH_TEST_PHASE ?? '') !== '';
  for (const file of fs.readdirSync(__dirname)) {
    if (!file.endsWith('.test.js')) continue;
    if ((file === 'crossProcess.test.js') === phased) mocha.addFile(path.join(__dirname, file));
  }
  return new Promise((resolve, reject) => {
    mocha.run((failures) => {
      if (failures > 0) reject(new Error(`${failures} test(s) failed`));
      else resolve();
    });
  });
}
