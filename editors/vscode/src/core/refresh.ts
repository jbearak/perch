// SPDX-License-Identifier: GPL-3.0-only

// When the session list refreshes: on activation, when the window gains
// focus, when the view becomes visible, after the extension's own actions,
// every 5 seconds while the view is visible and every 30 seconds while it is
// hidden. One refresh runs at a time; requests made during one coalesce into
// a single follow-up.

export const VISIBLE_INTERVAL_MS = 5_000;
export const HIDDEN_INTERVAL_MS = 30_000;

export interface Timers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export const realTimers: Timers = {
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

export class RefreshScheduler {
  private visible = false;
  private timer: unknown;
  private running = false;
  private again = false;
  private disposed = false;
  private pending: Promise<void> | undefined;

  constructor(
    private readonly refresh: () => Promise<void>,
    private readonly timers: Timers = realTimers,
  ) {}

  /** Starts the cadence with a refresh now (activation). */
  start(visible: boolean): void {
    this.visible = visible;
    this.now();
  }

  /** The view was shown or hidden. Showing it refreshes now. */
  setVisible(visible: boolean): void {
    if (visible === this.visible) return;
    this.visible = visible;
    if (visible) this.now();
    else this.schedule();
  }

  /** The window gained focus. */
  focused(): void {
    this.now();
  }

  /** Refresh now: after an action, on focus, or on demand. */
  now(): Promise<void> {
    if (this.disposed) return Promise.resolve();
    if (this.running) {
      this.again = true;
      return this.pending ?? Promise.resolve();
    }
    this.clear();
    this.running = true;
    this.pending = this.run();
    return this.pending;
  }

  private async run(): Promise<void> {
    try {
      do {
        this.again = false;
        try {
          await this.refresh();
        } catch {
          // The refresh reports its own errors; the cadence goes on.
        }
      } while (this.again && !this.disposed);
    } finally {
      this.running = false;
      this.pending = undefined;
      this.schedule();
    }
  }

  /** The current interval. */
  interval(): number {
    return this.visible ? VISIBLE_INTERVAL_MS : HIDDEN_INTERVAL_MS;
  }

  private schedule(): void {
    if (this.disposed || this.running) return;
    this.clear();
    this.timer = this.timers.setTimeout(() => {
      this.timer = undefined;
      void this.now();
    }, this.interval());
  }

  private clear(): void {
    if (this.timer !== undefined) {
      this.timers.clearTimeout(this.timer);
      this.timer = undefined;
    }
  }

  dispose(): void {
    this.disposed = true;
    this.clear();
  }
}
