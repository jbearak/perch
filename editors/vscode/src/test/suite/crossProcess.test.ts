// SPDX-License-Identifier: GPL-3.0-only

// Restore across a real window restart: runTest.ts launches VS Code twice
// with the same user data directory and workspace. The first launch creates
// a session in a terminal and exits, which detaches it; the second restores
// it from the restore set. The session ID goes through a marker file.
//
// The first launch ends when its tests do, which closes the window under
// the terminal: its attach process gets SIGHUP and detaches, and the session
// must stay in the restore set.
import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import { activated, bundledBinaryOrSkip, info, perch, waitFor } from './helpers';

const phase = process.env.PERCH_TEST_PHASE;
const marker = path.join(process.env.PERCH_TEST_SHARED ?? '/nonexistent', 'restore-session');

describe(`cross-process restore (${phase})`, () => {
  if (phase === 'create') {
    it('creates a session that the next window restores', async function () {
      const { ext, testing } = await activated();
      bundledBinaryOrSkip(this, ext.extensionPath);
      await testing.terminalsStarted;
      const { session } = await testing.createSession({ name: 'e2e restore', kind: 'shell', command: ['sleep', '600'] });
      assert.ok(testing.restoreSet().includes(session.id));
      fs.writeFileSync(marker, session.id);
    });
  }

  if (phase === 'restore') {
    it('restores the session the previous window had open', async function () {
      const { ext, testing } = await activated();
      const binary = bundledBinaryOrSkip(this, ext.extensionPath);
      assert.ok(fs.existsSync(marker), 'the first phase left its marker');
      const id = fs.readFileSync(marker, 'utf8').trim();
      try {
        await testing.terminalsStarted;
        const t = await waitFor('the restored terminal', async () => testing.terminalFor(id), 30_000);
        assert.strictEqual(t.name, 'e2e restore');
        await waitFor('the session attached here', async () => {
          const i = await info(binary, id);
          return i?.attached === true && i.clientLabel === testing.label ? true : undefined;
        }, 30_000);
      } finally {
        await perch(binary, ['kill', '--force', id]).catch(() => undefined);
      }
    });
  }
});
