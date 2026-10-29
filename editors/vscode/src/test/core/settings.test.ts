// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { defaults, readSettings } from '../../core/settings';

describe('settings', () => {
  it('reads perch.path and perch.addToPath', () => {
    const values: Record<string, unknown> = { path: ' /opt/perch ', addToPath: false };
    assert.deepStrictEqual(readSettings((k) => values[k]), { path: '/opt/perch', addToPath: false });
  });

  it('falls back to the defaults', () => {
    assert.deepStrictEqual(readSettings(() => undefined), defaults);
    assert.deepStrictEqual(readSettings(() => 42), { path: '', addToPath: true });
  });
});
