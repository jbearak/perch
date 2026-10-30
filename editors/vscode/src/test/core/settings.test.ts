// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { defaults, readSettings } from '../../core/settings';

describe('settings', () => {
  it('reads every perch.* setting', () => {
    const values: Record<string, unknown> = {
      path: ' /opt/perch ',
      addToPath: false,
      restoreTerminals: false,
      linkedEnvironment: ['SSH_AUTH_SOCK', 'SSH_AUTH_SOCK', 'not a name', 3],
      shell: ['/bin/zsh', '-l'],
    };
    assert.deepStrictEqual(readSettings((k) => values[k]), {
      path: '/opt/perch',
      addToPath: false,
      restoreTerminals: false,
      linkedEnvironment: ['SSH_AUTH_SOCK'],
      shell: ['/bin/zsh', '-l'],
    });
  });

  it('falls back to the defaults', () => {
    assert.deepStrictEqual(readSettings(() => undefined), defaults);
    assert.deepStrictEqual(readSettings(() => 42), defaults);
    assert.deepStrictEqual(readSettings((k) => (k === 'shell' ? [1] : undefined)).shell, '');
  });
});
