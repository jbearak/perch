// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { declaredLaunchers } from '../../core/launchers';

describe('launchers', () => {
  it('reads launchers from extension manifests, with defaults', () => {
    const logs: string[] = [];
    const list = declaredLaunchers(
      [
        { id: 'a.none', packageJSON: { name: 'none' } },
        {
          id: 'a.repl',
          packageJSON: {
            displayName: 'A REPL',
            perch: {
              launchers: [
                { label: 'REPL console', command: 'a.repl.new', description: 'A', icon: 'flame' },
                { label: 'Other', command: 'a.repl.other', icon: 'Not An Icon' },
                { label: '', command: 'x' },
                { label: 'No command' },
                null,
              ],
            },
          },
        },
        { id: 'b.localized', packageJSON: { displayName: '%displayName%', perch: { launchers: [{ label: 'B', command: 'b.new' }] } } },
        { id: 'c.bad', packageJSON: { perch: { launchers: { label: 'C' } } } },
      ],
      (m) => logs.push(m),
    );
    assert.deepStrictEqual(list, [
      { label: 'REPL console', description: 'A', icon: 'flame', command: 'a.repl.new', extensionId: 'a.repl' },
      { label: 'Other', description: 'A REPL', icon: 'terminal', command: 'a.repl.other', extensionId: 'a.repl' },
      { label: 'B', description: 'b.localized', icon: 'terminal', command: 'b.new', extensionId: 'b.localized' },
    ]);
    assert.strictEqual(logs.length, 4, logs.join('\n'));
  });
});
