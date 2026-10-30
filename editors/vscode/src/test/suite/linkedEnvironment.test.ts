// SPDX-License-Identifier: GPL-3.0-only

// The local checks for the linked-variable defaults. Each variable is
// listed in `perch.linkedEnvironment` here. `SSH_AUTH_SOCK` is a default:
// this check and the agent-forwarding check over ssh (tests/linux) pass.
// `VSCODE_IPC_HOOK_CLI` also needs its Remote-SSH check before it can be.
import * as assert from 'assert';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as net from 'net';
import * as path from 'path';
import * as vscode from 'vscode';
import type { Testing } from '../../vscode/activation';
import { activated, bundledBinaryOrSkip, info, perch, waitFor } from './helpers';

async function linkOnly(names: string[] | undefined): Promise<void> {
  await vscode.workspace.getConfiguration('perch').update('linkedEnvironment', names, vscode.ConfigurationTarget.Global);
}

/** A file the test's programs write whole: to a temporary name, then renamed. */
function readWhenThere(file: string): Promise<string> {
  return waitFor(`${file}`, async () => (fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : undefined));
}

function connects(socket: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const c = net.connect(socket, () => {
      c.end();
      resolve();
    });
    c.on('error', reject);
  });
}

function sessionDir(i: Record<string, unknown> | undefined): string {
  assert.ok(i && typeof i.dir === 'string', 'info has the session directory');
  return i.dir as string;
}

describe('linked variables', () => {
  let testing: Testing;
  let binary: string;
  let tmp: string;
  const ids: string[] = [];
  const terminals: vscode.Terminal[] = [];

  before(async function () {
    const a = await activated();
    testing = a.testing;
    binary = bundledBinaryOrSkip(this, a.ext.extensionPath);
    await testing.terminalsStarted;
    tmp = fs.mkdtempSync('/tmp/perch-link-');
  });

  afterEach(async () => {
    await linkOnly(undefined);
  });

  after(async () => {
    for (const t of terminals) t.dispose();
    for (const id of ids) await perch(binary, ['kill', '--force', id]).catch(() => undefined);
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  it('neither SSH_AUTH_SOCK nor VSCODE_IPC_HOOK_CLI is linked by default', async () => {
    const { options } = testing.prepareLaunch({ name: 'x', kind: 'x', command: ['true'] });
    assert.ok(!(options.shellArgs as string[]).includes('--link-env'));
  });

  it('VSCODE_IPC_HOOK_CLI: the variable is the link, it resolves to the window’s socket, and connect() reaches it', async () => {
    // A local window has no CLI socket: VS Code's extension host creates
    // one, and sets VSCODE_IPC_HOOK_CLI for terminals, only in a remote
    // window. So two sockets this test listens on stand in for two windows'
    // CLI servers, each passed to its terminal as VS Code would pass it.
    // The real socket is the Remote-SSH check in docs/verification.md.
    const windows = [path.join(tmp, 'w1.sock'), path.join(tmp, 'w2.sock')];
    const connections = [0, 0];
    const servers = windows.map((sock, i) => {
      const server = net.createServer((c) => {
        connections[i]++;
        c.end();
      });
      server.listen(sock);
      return server;
    });
    try {
      await linkOnly(['VSCODE_IPC_HOOK_CLI']);
      const out = path.join(tmp, 'session-ipc');
      const { id, options } = testing.prepareLaunch({
        name: 'ipc link',
        kind: 'test',
        command: ['sh', '-c', 'printf %s "$VSCODE_IPC_HOOK_CLI" > "$0.tmp"; mv "$0.tmp" "$0"; exec sleep 600', out],
      });
      ids.push(id);
      assert.ok((options.shellArgs as string[]).join(' ').includes('--link-env VSCODE_IPC_HOOK_CLI'));
      const first = vscode.window.createTerminal({ ...options, env: { VSCODE_IPC_HOOK_CLI: windows[0] } });
      terminals.push(first);
      const link = path.join(sessionDir(await waitFor('the session', () => info(binary, id))), 'env', 'VSCODE_IPC_HOOK_CLI');
      assert.strictEqual(await readWhenThere(out), link, 'the variable is the link');
      assert.strictEqual(fs.readlinkSync(link), windows[0], 'the link resolves to the window’s socket');
      await connects(link);
      await waitFor('the first window to see the connection', async () => (connections[0] === 1 ? true : undefined));
      assert.strictEqual(connections[1], 0);

      // The session moves to the second window: the same path now reaches its socket.
      first.dispose();
      await waitFor('the session to detach', async () => ((await info(binary, id))?.attached === false ? true : undefined));
      const second = vscode.window.createTerminal({
        name: 'ipc link again',
        shellPath: binary,
        shellArgs: ['attach', id, '--label', testing.label, '--detach-key', 'none'],
        env: { VSCODE_IPC_HOOK_CLI: windows[1] },
      });
      terminals.push(second);
      await waitFor('the link to follow the new window', async () => (fs.readlinkSync(link) === windows[1] ? true : undefined));
      await connects(link);
      await waitFor('the second window to see the connection', async () => (connections[1] === 1 ? true : undefined));
      assert.strictEqual(connections[0], 1);
    } finally {
      for (const server of servers) server.close();
    }
  });

  it('SSH_AUTH_SOCK: the link follows the attached client’s agent', async function () {
    const tools = ['ssh-agent', 'ssh-add', 'ssh-keygen'];
    const missing = tools.filter((t) => {
      try {
        execFileSync('sh', ['-c', `command -v ${t}`]);
        return false;
      } catch {
        return true;
      }
    });
    if (missing.length > 0) {
      if (process.env.PERCH_REQUIRE_BINARY === '1') assert.fail(`missing ${missing.join(', ')}`);
      this.skip();
    }
    // Two agents; only the first holds a key.
    const agents = [path.join(tmp, 'a1'), path.join(tmp, 'a2')];
    const pids: string[] = [];
    try {
      for (const a of agents) {
        const out = execFileSync('ssh-agent', ['-s', '-a', a], { encoding: 'utf8' });
        const pid = /SSH_AGENT_PID=(\d+)/.exec(out)?.[1];
        if (pid !== undefined) pids.push(pid);
      }
      const key = path.join(tmp, 'key');
      execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-C', 'perch-e2e-key', '-f', key]);
      execFileSync('ssh-add', ['-q', key], { env: { ...process.env, SSH_AUTH_SOCK: agents[0] } });

      await linkOnly(['SSH_AUTH_SOCK']);
      const varOut = path.join(tmp, 'ssh-var');
      const listed = path.join(tmp, 'ssh-list');
      const script =
        'printf %s "$SSH_AUTH_SOCK" > "$0.tmp"; mv "$0.tmp" "$0"; ' +
        'while :; do ssh-add -l > "$1.tmp" 2>&1; mv "$1.tmp" "$1"; sleep 0.2; done';
      const { id, options } = testing.prepareLaunch({ name: 'ssh link', kind: 'test', command: ['sh', '-c', script, varOut, listed] });
      ids.push(id);
      const first = vscode.window.createTerminal({ ...options, env: { SSH_AUTH_SOCK: agents[0] } });
      terminals.push(first);
      const link = path.join(sessionDir(await waitFor('the session', () => info(binary, id))), 'env', 'SSH_AUTH_SOCK');
      assert.strictEqual(await readWhenThere(varOut), link, 'the variable is the link');
      await waitFor('the first agent’s key', async () =>
        fs.existsSync(listed) && fs.readFileSync(listed, 'utf8').includes('perch-e2e-key') ? true : undefined,
      );
      assert.strictEqual(fs.readlinkSync(link), agents[0]);

      // Detach, then attach from a client with the other agent.
      first.dispose();
      await waitFor('the session to detach', async () => ((await info(binary, id))?.attached === false ? true : undefined));
      const second = vscode.window.createTerminal({
        name: 'ssh link again',
        shellPath: binary,
        shellArgs: ['attach', id, '--label', testing.label, '--detach-key', 'none'],
        env: { SSH_AUTH_SOCK: agents[1] },
      });
      terminals.push(second);
      await waitFor('the link to follow the new client', async () =>
        fs.readlinkSync(link) === agents[1] ? true : undefined,
      );
      await waitFor('ssh-add to reach the second agent', async () => {
        const text = fs.existsSync(listed) ? fs.readFileSync(listed, 'utf8') : '';
        return !text.includes('perch-e2e-key') && /no identities/i.test(text) ? true : undefined;
      });
      await connects(link);
    } finally {
      for (const pid of pids) {
        try {
          process.kill(Number(pid));
        } catch {
          // Already gone.
        }
      }
    }
  });
});
