// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import {
  commandLaunch,
  literal,
  profileKeys,
  profileLaunch,
  profileLaunches,
  ProfileHost,
  resolveExecutable,
  resolveVariables,
} from '../../core/profiles';

const executables = new Set(['/bin/zsh', '/usr/bin/bash', '/opt/homebrew/bin/fish', '/home/me/bin/{odd}']);

function host(o: Partial<ProfileHost> = {}): ProfileHost {
  return {
    env: { PATH: '/usr/local/bin:relative:/usr/bin:/opt/homebrew/bin', HOME: '/home/me', TOKEN: 'a{b}c' },
    home: '/home/me',
    workspaceFolder: '/home/me/project',
    isExecutable: (f) => executables.has(f),
    ...o,
  };
}

describe('profiles', () => {
  it('names the settings for macOS and Linux only', () => {
    assert.deepStrictEqual(profileKeys('darwin'), { profiles: 'profiles.osx', defaultProfile: 'defaultProfile.osx' });
    assert.deepStrictEqual(profileKeys('linux'), { profiles: 'profiles.linux', defaultProfile: 'defaultProfile.linux' });
    assert.strictEqual(profileKeys('win32'), undefined);
  });

  it('doubles braces so perch new passes them through', () => {
    assert.strictEqual(literal('{dir} {id} { } ${HOME}'), '{{dir}} {{id}} {{ }} ${{HOME}}');
    assert.strictEqual(literal('plain'), 'plain');
  });

  it('resolves the variables that mean the same outside VS Code', () => {
    const h = host();
    assert.strictEqual(resolveVariables('${env:HOME}/x', h), '/home/me/x');
    assert.strictEqual(resolveVariables('${env:MISSING}', h), '');
    assert.strictEqual(resolveVariables('${userHome}${/}a${pathSeparator}b', h), '/home/me/a/b');
    assert.strictEqual(resolveVariables('${workspaceFolder}:${workspaceFolderBasename}', h), '/home/me/project:project');
    assert.strictEqual(resolveVariables('${workspaceFolder}', host({ workspaceFolder: undefined })), undefined);
    assert.strictEqual(resolveVariables('${command:pick}', h), undefined);
    assert.strictEqual(resolveVariables('${config:x}', h), undefined);
    assert.strictEqual(resolveVariables('${file}', h), undefined);
    assert.strictEqual(resolveVariables('${workspaceFolder:other}', h), undefined);
    assert.strictEqual(resolveVariables('no variables', h), 'no variables');
    // Not a VS Code variable: left for the program, as VS Code leaves it.
    assert.strictEqual(resolveVariables('echo ${HOME} ${X-unset} ${env:HOME}', h), 'echo ${HOME} ${X-unset} /home/me');
  });

  it('finds absolute paths and bare names on PATH', () => {
    const h = host();
    assert.strictEqual(resolveExecutable('/bin/zsh', h), '/bin/zsh');
    assert.strictEqual(resolveExecutable('/bin/nope', h), undefined);
    assert.strictEqual(resolveExecutable('bash', h), '/usr/bin/bash');
    assert.strictEqual(resolveExecutable('fish', h), '/opt/homebrew/bin/fish');
    assert.strictEqual(resolveExecutable('pwsh', h), undefined);
    assert.strictEqual(resolveExecutable('bin/zsh', h), undefined, 'relative with a slash');
    assert.strictEqual(resolveExecutable('', h), undefined);
    assert.strictEqual(resolveExecutable('bash', host({ env: {} })), undefined, 'no PATH');
  });

  it('turns a profile into a perch new command', () => {
    const p = profileLaunch('zsh', { path: 'zsh', args: ['-l'], icon: 'terminal-bash', color: 'terminal.ansiGreen' }, host());
    assert.strictEqual(p, undefined, 'zsh is not on PATH here');
    const z = profileLaunch(
      'login zsh',
      { path: ['/bin/nope', '/bin/zsh'], args: ['-l', '-c', 'echo {x} ${env:TOKEN}'], icon: 'terminal-bash', color: 'terminal.ansiGreen' },
      host(),
    );
    assert.deepStrictEqual(z, {
      name: 'login zsh',
      kind: 'shell',
      command: ['/bin/zsh', '-l', '-c', 'echo {{x}} a{{b}}c'],
      env: undefined,
      icon: 'terminal-bash',
      color: 'terminal.ansiGreen',
      display: '/bin/zsh -l -c echo {x} a{b}c',
    });
  });

  it('escapes braces in the path, the arguments and the environment', () => {
    const p = profileLaunch('odd', { path: '/home/me/bin/{odd}', args: ['{dir}', '}{'], env: { A: '{id}', B: '${env:TOKEN}' } }, host());
    assert.deepStrictEqual(p?.command, ['/home/me/bin/{{odd}}', '{{dir}}', '}}{{']);
    assert.deepStrictEqual(p?.env, { A: '{{id}}', B: 'a{{b}}c' });
  });

  it('unsets a null variable with env -u and sets the rest', () => {
    const p = profileLaunch('bash', { path: 'bash', env: { KEEP: 'yes', DROP: null, ALSO: null, 'BAD=NAME': 'x', N: 3 } }, host());
    assert.deepStrictEqual(p?.command, ['/usr/bin/env', '-u', 'DROP', '-u', 'ALSO', '/usr/bin/bash']);
    assert.deepStrictEqual(p?.env, { KEEP: 'yes' });
    assert.strictEqual(p?.display, '/usr/bin/bash');
  });

  it('takes a lone string argument as one argument', () => {
    assert.deepStrictEqual(profileLaunch('b', { path: 'bash', args: '--norc' }, host())?.command, ['/usr/bin/bash', '--norc']);
  });

  it('skips profiles it cannot run', () => {
    const h = host();
    assert.strictEqual(profileLaunch('removed', null, h), undefined);
    assert.strictEqual(profileLaunch('ext', { extensionIdentifier: 'a.b', id: 'c' }, h), undefined);
    assert.strictEqual(profileLaunch('win', { source: 'PowerShell' }, h), undefined);
    assert.strictEqual(profileLaunch('missing', { path: 'pwsh' }, h), undefined);
    assert.strictEqual(profileLaunch('cmd', { path: '/bin/zsh', args: ['${command:x}'] }, h), undefined);
    assert.strictEqual(profileLaunch('cfg', { path: '/bin/zsh', env: { A: '${config:x}' } }, h), undefined);
    assert.strictEqual(profileLaunch('pathvar', { path: '${command:x}' }, h), undefined);
    assert.strictEqual(
      profileLaunch('ws', { path: '/bin/zsh', args: ['${workspaceFolder}'] }, host({ workspaceFolder: undefined })),
      undefined,
    );
  });

  it('falls back from an unknown icon and a non-terminal color', () => {
    const p = profileLaunch('z', { path: '/bin/zsh', icon: { id: 'Not An Icon' }, color: 'editor.foreground' }, host());
    assert.strictEqual(p?.icon, undefined);
    assert.strictEqual(p?.color, undefined);
    assert.strictEqual(profileLaunch('z', { path: '/bin/zsh', icon: { id: 'flame' } }, host())?.icon, 'flame');
  });

  it('lists the runnable profiles in settings order', () => {
    const list = profileLaunches(
      { bash: { path: 'bash' }, zsh: { path: 'zsh' }, removed: null, mine: { path: '/bin/zsh', args: ['-i'] } },
      host(),
    );
    assert.deepStrictEqual(list.map((p) => p.name), ['bash', 'mine']);
    assert.deepStrictEqual(profileLaunches(undefined, host()), []);
  });

  it('runs a typed command line with sh -c, braces kept', () => {
    assert.deepStrictEqual(commandLaunch("  awk '{print $1}' {dir}  "), {
      name: "awk '{print $1}' {dir}",
      kind: '',
      command: ['/bin/sh', '-c', "awk '{{print $1}}' {{dir}}"],
      display: "awk '{print $1}' {dir}",
    });
    const long = commandLaunch('x'.repeat(50));
    assert.strictEqual(long.name.length, 40);
    assert.ok(long.name.endsWith('…'));
  });
});
