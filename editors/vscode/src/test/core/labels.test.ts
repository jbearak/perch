// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { createHash } from 'crypto';
import { clientLabel, windowKey, workspaceKey, workspaceKeyOf } from '../../core/labels';

describe('labels', () => {
  it('keys a workspace by the first 16 hex digits of its URI hash', () => {
    const uri = 'file:///home/me/thesis';
    const full = createHash('sha256').update(uri).digest('hex');
    assert.strictEqual(workspaceKey(uri), full.slice(0, 16));
    assert.match(workspaceKey(uri), /^[0-9a-f]{16}$/);
    assert.notStrictEqual(workspaceKey(uri), workspaceKey('file:///home/me/other'));
  });

  it('uses none without a workspace', () => {
    assert.strictEqual(workspaceKey(undefined), 'none');
    assert.strictEqual(workspaceKey(''), 'none');
  });

  it('makes 8 random hex digits per window', () => {
    assert.strictEqual(windowKey(() => Buffer.from([0xde, 0xad, 0xbe, 0xef])), 'deadbeef');
    const a = windowKey();
    assert.match(a, /^[0-9a-f]{8}$/);
    assert.notStrictEqual(a, windowKey());
  });

  it('joins and splits labels', () => {
    const label = clientLabel('0123456789abcdef', 'deadbeef');
    assert.strictEqual(label, '0123456789abcdef#deadbeef');
    assert.strictEqual(workspaceKeyOf(label), '0123456789abcdef');
    assert.strictEqual(workspaceKeyOf('none#00000000'), 'none');
    for (const bad of [undefined, '', 'perch-cli', '0123#deadbeef', '0123456789abcdef#dead']) {
      assert.strictEqual(workspaceKeyOf(bad), undefined, String(bad));
    }
  });
});
