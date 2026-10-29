// SPDX-License-Identifier: GPL-3.0-only

// The stable copy of perch for terminals and plain SSH: installed into
// global storage by `perch self-install`, then appended to `PATH` and named
// in `PERCH_BIN` for new terminals, under `perch.addToPath`.
import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { pathAppendValue } from '../core/binary';
import { PerchCli } from '../core/cli';

export class CommandLine {
  /** Whether the copy exists after the last install attempt. */
  private ready = false;

  constructor(
    /** `<globalStorage>/bin/<os>-<arch>`. */
    readonly dir: string,
    private readonly env: vscode.EnvironmentVariableCollection,
    private readonly output: vscode.OutputChannel,
  ) {}

  get binary(): string {
    return path.join(this.dir, 'perch');
  }

  /** Installs or updates the copy, then applies `addToPath`. */
  async install(cli: PerchCli, addToPath: boolean): Promise<void> {
    try {
      const r = await cli.selfInstall(this.dir);
      const how = r.locked ? '' : ' (without a lock: flock is not supported here)';
      this.output.appendLine(r.installed ? `Installed ${r.path}${how}` : `${r.path} is current`);
    } catch (err) {
      this.output.appendLine(`Could not install the command-line copy: ${String(err)}`);
    }
    this.ready = fs.existsSync(this.binary);
    this.apply(addToPath);
  }

  /** Sets or clears the terminal environment. */
  apply(addToPath: boolean): void {
    this.env.clear();
    if (!addToPath || !this.ready) return;
    this.env.description = "Adds Perch's command-line copy to PATH (setting perch.addToPath)";
    this.env.append('PATH', pathAppendValue(this.dir));
    this.env.replace('PERCH_BIN', this.binary);
  }

  /** "Perch: Show Command-Line Path". */
  async show(): Promise<void> {
    if (!fs.existsSync(this.binary)) {
      void vscode.window.showWarningMessage(
        `Perch's command-line copy is not installed yet (expected in ${this.dir}). See the Perch output for why.`,
      );
      this.output.show(true);
      return;
    }
    this.output.appendLine(`Command-line path: ${this.dir}`);
    this.output.appendLine(`To use perch over plain SSH, add to your shell's startup file: export PATH="$PATH:${this.dir}"`);
    const copy = 'Copy Path';
    const choice = await vscode.window.showInformationMessage(
      `perch is in ${this.dir}. New terminals here have it on PATH when perch.addToPath is on; ` +
        'for plain SSH sessions, add this directory to PATH in your shell startup file.',
      copy,
    );
    if (choice === copy) await vscode.env.clipboard.writeText(this.dir);
  }
}
