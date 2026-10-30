// SPDX-License-Identifier: GPL-3.0-only

// The API for other extensions. `exports.getApi(1, callerId)` returns a
// PerchApiV1 whose sessions record `callerId` as their owner; any other
// version returns undefined, and callers carry on without Perch.
//
// `send` is the one operation that puts input into a session, and only
// when the caller asks. Nothing else here types into a session.
import * as vscode from 'vscode';
import {
  API_VERSIONS,
  apiError,
  EndReporter,
  errorCode,
  isAttachedHere,
  matchesFilter,
  PerchApiError,
  sendErrorCode,
  SessionInfo,
  startingInfo,
  toSessionInfo,
} from '../core/api';
import { PerchCli } from '../core/cli';
import { isEnded, ListDiff, Session } from '../core/sessions';
import { CloseOutcome, Terminals } from './terminals';

export type { SessionInfo, ApiErrorCode } from '../core/api';

export interface CreateOptions {
  name: string;
  /** `r`, `stata`, `shell`, ... */
  kind: string;
  /** `{dir}` and `{id}` expand. */
  command: string[];
  cwd?: string;
  /** `{dir}` and `{id}` expand. */
  env?: Record<string, string>;
  annotations?: Record<string, string>;
  terminal?: {
    location?: vscode.TerminalLocation;
    // `vscode.IconPath` is not in the 1.82 typings.
    iconPath?: vscode.TerminalOptions['iconPath'];
    color?: vscode.ThemeColor;
  };
}

export interface PerchApiV1 {
  readonly version: 1;
  /** False on Windows hosts or when the binary cannot run. */
  available(): Promise<boolean>;
  /** The launch for a new session, without creating a terminal. */
  prepareLaunch(options: CreateOptions): { id: string; options: vscode.TerminalOptions };
  /** Creates the terminal and resolves once the program is running. */
  createSession(
    options: CreateOptions & { show?: boolean },
  ): Promise<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Resolves once a terminal from prepareLaunch has a running session. */
  waitForSession(id: string, terminal: vscode.Terminal): Promise<SessionInfo>;
  listSessions(filter?: { owner?: string; kind?: string }): Promise<SessionInfo[]>;
  /** Perch terminals open in this window, with their sessions. Call on activation. */
  terminals(): ReadonlyArray<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Opens (or shows) a terminal attached to the session. */
  attach(id: string, options?: { show?: boolean }): Promise<vscode.Terminal>;
  sessionForTerminal(terminal: vscode.Terminal): SessionInfo | undefined;
  /**
   * Queues text for the session's program, as if typed. Rejects with a
   * PerchApiError whose `code` is `queue_full`, `outcome_unknown` (it may
   * have been queued: never resend), `no_session` or `unavailable`.
   */
  send(id: string, text: string, options?: { paste?: boolean; submit?: boolean }): Promise<'queued'>;
  kill(id: string, options?: { force?: boolean }): Promise<void>;
  annotate(id: string, set: Record<string, string>, unset?: string[]): Promise<void>;
  readonly onDidChangeSessions: vscode.Event<void>;
  /** Fires whenever Perch attaches a terminal to a session: create, open, restore, adopt. */
  readonly onDidAttach: vscode.Event<{ session: SessionInfo; terminal: vscode.Terminal }>;
  /** Fires once when Perch sees that a session has ended: exited, killed or lost. */
  readonly onDidEndSession: vscode.Event<SessionInfo>;
}

export interface ApiDeps {
  /** A macOS or Linux host. */
  supported: boolean;
  cli(): PerchCli;
  /** The latest listing's view of a session. */
  find(id: string): Session | undefined;
  sessions(): readonly Session[];
  /** Lists again, and resolves once the listing is in. */
  refresh(): Promise<void>;
  terminals: Terminals;
  /** Where the binary keeps sessions, for `{dir}` before a listing shows one. */
  runtimeDir: string;
  /** Each listing that changed something. */
  onDidChangeList: vscode.Event<ListDiff>;
}

const unavailableMessage = 'Perch is not available on this host.';

/** The events every API object shares, and `getApi`. */
export class Api implements vscode.Disposable {
  private readonly changed = new vscode.EventEmitter<void>();
  private readonly attached = new vscode.EventEmitter<{ session: SessionInfo; terminal: vscode.Terminal }>();
  private readonly ended = new vscode.EventEmitter<SessionInfo>();
  private readonly reporter = new EndReporter();
  private readonly disposables: vscode.Disposable[];
  private readonly apis = new Map<string, PerchApiV1>();

  constructor(private readonly deps: ApiDeps) {
    this.disposables = [
      this.changed,
      this.attached,
      this.ended,
      deps.onDidChangeList((diff) => {
        this.changed.fire();
        for (const s of this.reporter.ended(diff.ended)) this.ended.fire(this.info(s));
      }),
      deps.terminals.onDidAttach(({ session, terminal }) => this.attached.fire({ session: this.info(session), terminal })),
      deps.terminals.onDidClose((o: CloseOutcome) => {
        // The program's exit, as this window's terminal received it.
        if (o.reason === 'process' && o.result !== 'busy') this.reporter.terminalExit(o.id, o.code);
      }),
    ];
  }

  /** `exports.getApi`: an API object for the version, or undefined. */
  getApi(version: number, callerId: string): PerchApiV1 | undefined {
    if (!API_VERSIONS.includes(version)) return undefined;
    const owner = typeof callerId === 'string' && callerId !== '' ? callerId : '';
    let api = this.apis.get(owner);
    if (api === undefined) {
      api = this.v1(owner === '' ? undefined : owner);
      this.apis.set(owner, api);
    }
    return api;
  }

  private info(s: Session): SessionInfo {
    return toSessionInfo(s, isAttachedHere(s, this.deps.terminals.labels()));
  }

  /** The listed session, or what this window's launch said about one not listed yet. */
  private infoFor(id: string): SessionInfo {
    const s = this.deps.find(id);
    if (s !== undefined) return this.info(s);
    return startingInfo(id, this.deps.runtimeDir, this.deps.terminals.launched(id));
  }

  private v1(owner: string | undefined): PerchApiV1 {
    const deps = this.deps;
    const terminals = deps.terminals;
    const requireSupported = (): void => {
      if (!deps.supported) throw new PerchApiError('unavailable', unavailableMessage);
    };
    const api: PerchApiV1 = {
      version: 1,
      available: async () => deps.supported && (await deps.cli().works()),
      prepareLaunch: (options) => {
        requireSupported();
        checkCreate(options);
        return terminals.prepareLaunch({ ...options, owner });
      },
      createSession: async (options) => {
        requireSupported();
        checkCreate(options);
        const { session, terminal } = await terminals.createSession({ ...options, owner });
        return { session: this.info(session), terminal };
      },
      waitForSession: async (id, terminal) => {
        requireSupported();
        return this.info(await terminals.waitForSession(id, terminal, terminal.name));
      },
      listSessions: async (filter) => {
        if (!deps.supported) return [];
        await deps.refresh();
        return deps
          .sessions()
          .map((s) => this.info(s))
          .filter((s) => matchesFilter(s, filter));
      },
      terminals: () =>
        terminals.registry
          .all()
          .filter(({ terminal }) => terminal.exitStatus === undefined)
          .map(({ terminal, launch }) => ({ session: this.infoFor(launch.id), terminal })),
      attach: async (id, options) => {
        requireSupported();
        let s: Session | undefined;
        try {
          s = await deps.cli().info(id);
        } catch (err) {
          throw apiError(err, errorCode(err));
        }
        if (s === undefined || isEnded(s)) {
          throw new PerchApiError('no_session', `Session ${id} is ${s === undefined ? 'gone' : 'no longer running'}.`);
        }
        const terminal = await terminals.open(s, options?.show !== false);
        if (terminal === undefined) throw new Error(`Opening ${id} here was declined.`);
        return terminal;
      },
      sessionForTerminal: (terminal) => {
        const launch = terminals.registry.get(terminal);
        return launch === undefined ? undefined : this.infoFor(launch.id);
      },
      send: async (id, text, options) => {
        requireSupported();
        if (typeof text !== 'string') throw new TypeError('send: the text must be a string');
        try {
          await deps.cli().send(id, text, { paste: options?.paste === true, submit: options?.submit === true });
        } catch (err) {
          throw apiError(err, sendErrorCode(err));
        }
        return 'queued';
      },
      kill: async (id, options) => {
        requireSupported();
        try {
          await deps.cli().kill(id, options?.force === true);
        } catch (err) {
          throw apiError(err, errorCode(err));
        } finally {
          await deps.refresh();
        }
      },
      annotate: async (id, set, unset = []) => {
        requireSupported();
        if (Object.keys(set).length === 0 && unset.length === 0) return;
        try {
          await deps.cli().annotate(id, set, unset);
        } catch (err) {
          throw apiError(err, errorCode(err));
        }
        await deps.refresh();
      },
      onDidChangeSessions: this.changed.event,
      onDidAttach: this.attached.event,
      onDidEndSession: this.ended.event,
    };
    return Object.freeze(api);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}

function checkCreate(o: CreateOptions): void {
  if (typeof o !== 'object' || o === null) throw new TypeError('createSession: options are required');
  if (typeof o.name !== 'string' || typeof o.kind !== 'string') {
    throw new TypeError('createSession: name and kind must be strings');
  }
  if (!Array.isArray(o.command) || o.command.length === 0 || !o.command.every((a) => typeof a === 'string')) {
    throw new TypeError('createSession: command must be a non-empty array of strings');
  }
}
