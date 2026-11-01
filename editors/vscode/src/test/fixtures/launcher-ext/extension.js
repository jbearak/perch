// SPDX-License-Identifier: GPL-3.0-only

// A test extension that declares consoles for Perch's list. Its command
// starts one in a Perch session through Perch's API, as a real extension's
// launcher would.
const vscode = require('vscode');

exports.activate = (context) => {
  context.subscriptions.push(
    vscode.commands.registerCommand('perchTest.newConsole', async () => {
      const ext = vscode.extensions.getExtension('jbearak.perch');
      const exports_ = ext.isActive ? ext.exports : await ext.activate();
      const perch = exports_.getApi(1, 'perch-test.perch-launcher-fixture');
      await perch.createSession({ name: 'fixture console', kind: 'fixture', command: ['sleep', '600'] });
    }),
    vscode.commands.registerCommand('perchTest.reject', async () => {
      throw new Error('The fixture console could not start.');
    }),
    vscode.commands.registerCommand('perchTest.cancel', async () => {
      throw new vscode.CancellationError();
    }),
  );
};
