// SPDX-License-Identifier: GPL-3.0-only

// The VS Code side of activation. Sessions, terminals and the API arrive in
// later milestones.
import * as vscode from 'vscode';

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel('Perch');
  context.subscriptions.push(output);
}

export function deactivate(): void {}
