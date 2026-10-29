// SPDX-License-Identifier: GPL-3.0-only

import * as assert from 'assert';
import { HIDDEN_INTERVAL_MS, RefreshScheduler, Timers, VISIBLE_INTERVAL_MS } from '../../core/refresh';

/** A clock that only moves when told to. */
class FakeTimers implements Timers {
  now = 0;
  private next = 1;
  private readonly pending = new Map<number, { at: number; fn: () => void }>();

  setTimeout(fn: () => void, ms: number): unknown {
    const id = this.next++;
    this.pending.set(id, { at: this.now + ms, fn });
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.pending.delete(handle as number);
  }

  /** The delays of the timers set, from now. */
  delays(): number[] {
    return [...this.pending.values()].map((t) => t.at - this.now);
  }

  async advance(ms: number): Promise<void> {
    const end = this.now + ms;
    for (;;) {
      const due = [...this.pending.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (due === undefined) break;
      this.pending.delete(due[0]);
      this.now = due[1].at;
      due[1].fn();
      await settle();
    }
    this.now = end;
  }
}

async function settle(): Promise<void> {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

function setup(): { timers: FakeTimers; calls: number[]; scheduler: RefreshScheduler } {
  const timers = new FakeTimers();
  const calls: number[] = [];
  const scheduler = new RefreshScheduler(async () => {
    calls.push(timers.now);
  }, timers);
  return { timers, calls, scheduler };
}

describe('refresh cadence', () => {
  it('refreshes on activation, then every 5 seconds while visible', async () => {
    const { timers, calls, scheduler } = setup();
    scheduler.start(true);
    await settle();
    assert.deepStrictEqual(calls, [0]);
    assert.deepStrictEqual(timers.delays(), [VISIBLE_INTERVAL_MS]);
    await timers.advance(15_000);
    assert.deepStrictEqual(calls, [0, 5_000, 10_000, 15_000]);
    scheduler.dispose();
  });

  it('refreshes every 30 seconds while hidden', async () => {
    const { timers, calls, scheduler } = setup();
    scheduler.start(false);
    await settle();
    assert.deepStrictEqual(timers.delays(), [HIDDEN_INTERVAL_MS]);
    await timers.advance(60_000);
    assert.deepStrictEqual(calls, [0, 30_000, 60_000]);
    scheduler.dispose();
  });

  it('refreshes when the view becomes visible and slows down when hidden', async () => {
    const { timers, calls, scheduler } = setup();
    scheduler.start(false);
    await settle();
    await timers.advance(10_000);
    scheduler.setVisible(true);
    await settle();
    assert.deepStrictEqual(calls, [0, 10_000]);
    assert.deepStrictEqual(timers.delays(), [VISIBLE_INTERVAL_MS]);
    scheduler.setVisible(true);
    await settle();
    assert.strictEqual(calls.length, 2, 'no refresh when nothing changed');
    scheduler.setVisible(false);
    assert.deepStrictEqual(timers.delays(), [HIDDEN_INTERVAL_MS]);
    assert.strictEqual(calls.length, 2, 'hiding does not refresh');
    scheduler.dispose();
  });

  it('refreshes on focus and after actions, restarting the interval', async () => {
    const { timers, calls, scheduler } = setup();
    scheduler.start(true);
    await settle();
    await timers.advance(3_000);
    scheduler.focused();
    await settle();
    await timers.advance(1_000);
    await scheduler.now();
    assert.deepStrictEqual(calls, [0, 3_000, 4_000]);
    assert.deepStrictEqual(timers.delays(), [VISIBLE_INTERVAL_MS]);
    scheduler.dispose();
  });

  it('runs one refresh at a time and coalesces requests made during one', async () => {
    const timers = new FakeTimers();
    let running = 0;
    let max = 0;
    let count = 0;
    let release: () => void = () => {};
    const scheduler = new RefreshScheduler(async () => {
      running++;
      count++;
      max = Math.max(max, running);
      await new Promise<void>((r) => (release = r));
      running--;
    }, timers);
    const first = scheduler.now();
    const second = scheduler.now();
    void scheduler.now();
    release();
    await settle();
    release();
    await Promise.all([first, second]);
    assert.strictEqual(max, 1);
    assert.strictEqual(count, 2, 'one follow-up for any number of requests');
    assert.deepStrictEqual(timers.delays(), [HIDDEN_INTERVAL_MS]);
    scheduler.dispose();
  });

  it('keeps going after a failed refresh and stops when disposed', async () => {
    const timers = new FakeTimers();
    let count = 0;
    const scheduler = new RefreshScheduler(async () => {
      count++;
      throw new Error('boom');
    }, timers);
    scheduler.start(true);
    await settle();
    await timers.advance(5_000);
    assert.strictEqual(count, 2);
    scheduler.dispose();
    assert.deepStrictEqual(timers.delays(), []);
    await scheduler.now();
    assert.strictEqual(count, 2);
  });
});
