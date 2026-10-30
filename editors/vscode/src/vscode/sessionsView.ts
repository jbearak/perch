// SPDX-License-Identifier: GPL-3.0-only

// The "Perch" view: every session on this host for this user, and the
// status bar count of detached sessions.
import * as vscode from 'vscode';
import {
  countDetached,
  describe,
  DescribeContext,
  diffSessions,
  displayName,
  exitText,
  homeRelative,
  iconName,
  isEmptyDiff,
  isEnded,
  kindName,
  ListDiff,
  Session,
} from '../core/sessions';

/** The latest listing, and an event when it changes. */
export class SessionStore implements vscode.Disposable {
  private current: Session[] = [];
  private loaded = false;
  private readonly changed = new vscode.EventEmitter<ListDiff>();
  readonly onDidChange = this.changed.event;
  private readonly refreshed = new vscode.EventEmitter<void>();
  /** Fires after every listing, changed or not: ages move on. */
  readonly onDidRefresh = this.refreshed.event;
  private readonly listed = new vscode.EventEmitter<readonly Session[]>();
  /** Fires after every successful listing, with it. */
  readonly onDidList = this.listed.event;
  /** Why the last listing failed, or undefined. */
  error: string | undefined;

  get sessions(): readonly Session[] {
    return this.current;
  }

  find(id: string): Session | undefined {
    return this.current.find((s) => s.id === id);
  }

  /** Replaces the listing; fires if anything changed. */
  set(sessions: Session[], error?: string): void {
    const diff = diffSessions(this.current, sessions);
    const first = !this.loaded;
    const errorChanged = error !== this.error;
    this.loaded = true;
    this.current = sessions;
    this.error = error;
    if (first || errorChanged || !isEmptyDiff(diff)) this.changed.fire(diff);
    if (error === undefined) this.listed.fire(sessions);
    this.refreshed.fire();
  }

  dispose(): void {
    this.changed.dispose();
    this.listed.dispose();
    this.refreshed.dispose();
  }
}

export class SessionItem extends vscode.TreeItem {
  constructor(
    readonly session: Session,
    ctx: DescribeContext,
  ) {
    super(displayName(session), vscode.TreeItemCollapsibleState.None);
    this.id = session.id;
    this.description = describe(session, ctx);
    this.iconPath = new vscode.ThemeIcon(iconName(session));
    this.contextValue = isEnded(session) ? 'perch.session.ended' : 'perch.session.live';
    this.tooltip = tooltip(session, ctx);
  }
}

function tooltip(s: Session, ctx: DescribeContext): vscode.MarkdownString {
  const md = new vscode.MarkdownString();
  md.appendMarkdown(`**${escape(displayName(s))}** \`${s.id}\`\n\n`);
  md.appendMarkdown(`${escape(kindName(s.kind))} · ${escape(s.state)}`);
  if (s.state === 'exited') md.appendMarkdown(` (${escape(exitText(s.exit))})`);
  md.appendMarkdown('\n\n');
  if (s.cwd !== '') md.appendMarkdown(`Directory: ${escape(homeRelative(s.cwd, ctx.home))}\n\n`);
  if (s.owner !== undefined) md.appendMarkdown(`Created by ${escape(s.owner)}\n\n`);
  if (s.degraded.includes('directory-recreated')) {
    md.appendMarkdown(
      'Warning: the session directory was removed and recreated. Files other programs kept there are gone.\n\n',
    );
  }
  if (s.state === 'lost') md.appendMarkdown('Its supervisor ended unexpectedly.\n\n');
  return md;
}

function escape(text: string): string {
  return text.replace(/[\\`*_{}[\]()#+\-.!|<>]/g, '\\$&');
}

export class SessionsProvider implements vscode.TreeDataProvider<SessionItem>, vscode.Disposable {
  private readonly changed = new vscode.EventEmitter<SessionItem | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly subscription: vscode.Disposable;

  constructor(
    private readonly store: SessionStore,
    private readonly context: () => DescribeContext,
  ) {
    this.subscription = store.onDidRefresh(() => this.changed.fire(undefined));
  }

  getTreeItem(item: SessionItem): SessionItem {
    return item;
  }

  getChildren(item?: SessionItem): SessionItem[] {
    if (item !== undefined) return [];
    const ctx = this.context();
    // Live sessions first, newest first within each group.
    return [...this.store.sessions]
      .sort((a, b) => Number(isEnded(a)) - Number(isEnded(b)) || b.createdAt.localeCompare(a.createdAt))
      .map((s) => new SessionItem(s, ctx));
  }

  getParent(): undefined {
    return undefined;
  }

  item(id: string): SessionItem | undefined {
    const s = this.store.find(id);
    return s && new SessionItem(s, this.context());
  }

  dispose(): void {
    this.subscription.dispose();
    this.changed.dispose();
  }
}

/** The status bar count of detached sessions; hidden when there are none. */
export class DetachedCount implements vscode.Disposable {
  private readonly item = vscode.window.createStatusBarItem('perch.detached', vscode.StatusBarAlignment.Left, 50);
  private readonly subscription: vscode.Disposable;

  constructor(private readonly store: SessionStore) {
    this.item.name = 'Perch: Detached Sessions';
    this.item.command = 'perch.showDetached';
    this.subscription = store.onDidChange(() => this.update());
    this.update();
  }

  update(): void {
    const n = countDetached(this.store.sessions);
    this.item.text = `$(terminal) ${n}`;
    this.item.tooltip = `${n} detached Perch session${n === 1 ? '' : 's'}`;
    if (n > 0) this.item.show();
    else this.item.hide();
  }

  get count(): number {
    return countDetached(this.store.sessions);
  }

  dispose(): void {
    this.subscription.dispose();
    this.item.dispose();
  }
}
