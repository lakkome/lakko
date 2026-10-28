// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The debounced writer of the cross-device sync row (`sync-state-idb.ts`),
 * separated from `DeviceGroupSyncContext.tsx` so its lifecycle can be tested
 * without a component.
 *
 * A change schedules a write; the write runs {@link SyncStatePersister.schedule}'s
 * debounce later, capturing the state at that moment (a draft moves on every
 * keystroke, so the newest state is what is written). Log out tears the sync
 * runtime down, and {@link SyncStatePersister.flush} is how the teardown keeps
 * the last edit: it captures and writes a still-queued change at once instead
 * of dropping it with the timer. The capture runs synchronously inside
 * `flush`, so the teardown can free the wasm fold right after.
 *
 * The capture callback decides whether a write may happen at all: it returns
 * `null` once the account is no longer bound, and the persister then writes
 * nothing. Writes are serialized, so a slow earlier write can never land over
 * a later one.
 */

import type { SyncStateRow } from "./sync-state-store";

/** What a capture hands the writer: the row, and whatever the writer needs
 *  to know about the moment it was taken (the delete generation). */
export interface CapturedSyncState {
  readonly row: SyncStateRow;
  readonly generation: number;
}

/** Reads the state to write; `null` means "do not write" (the account is no
 *  longer bound, the device was removed, the row was wiped). */
export type SyncStateCapture = () => CapturedSyncState | null;

export interface SyncStatePersisterDeps {
  readonly debounceMs: number;
  /** Write one row. Errors are reported through `onError`, never thrown. */
  readonly save: (account: string, captured: CapturedSyncState) => Promise<void>;
  readonly onError?: (err: unknown) => void;
  /** Injected for tests; the platform timers by default. */
  readonly setTimer?: (fn: () => void, ms: number) => unknown;
  readonly clearTimer?: (handle: unknown) => void;
}

export class SyncStatePersister {
  private queued: { account: string; capture: SyncStateCapture } | null = null;
  private timer: unknown = null;
  private chain: Promise<void> = Promise.resolve();
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (handle: unknown) => void;

  constructor(private readonly deps: SyncStatePersisterDeps) {
    this.setTimer = deps.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer =
      deps.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  }

  /** Whether a change is waiting for its debounce. */
  get hasQueued(): boolean {
    return this.queued !== null;
  }

  /** Queue a write for `account`, restarting the debounce. A newer schedule
   *  replaces an older one: only the latest capture is ever written. */
  schedule(account: string, capture: SyncStateCapture): void {
    this.stopTimer();
    this.queued = { account, capture };
    this.timer = this.setTimer(() => {
      this.timer = null;
      void this.flush();
    }, this.deps.debounceMs);
  }

  /**
   * Write the queued change now, if any. The capture runs before this
   * returns; the returned promise settles when every write started so far
   * (this one included) has finished.
   */
  flush(): Promise<void> {
    this.stopTimer();
    const queued = this.queued;
    this.queued = null;
    if (queued !== null) {
      let captured: CapturedSyncState | null = null;
      try {
        captured = queued.capture();
      } catch (err) {
        this.deps.onError?.(err);
      }
      if (captured !== null) {
        const snapshot = captured;
        this.chain = this.chain.then(async () => {
          try {
            await this.deps.save(queued.account, snapshot);
          } catch (err) {
            this.deps.onError?.(err);
          }
        });
      }
    }
    return this.chain;
  }

  /**
   * Write `capture` now, after every write started so far, and reject when
   * it cannot be written: the capture throws or returns `null`, or the save
   * fails. For a change that must be on disk before anything depends on it
   * (an invite link's record, before the link is handed out). A change
   * waiting for its debounce is replaced: the capture reads the newest state.
   */
  async writeNow(account: string, capture: SyncStateCapture): Promise<void> {
    this.stopTimer();
    this.queued = null;
    const captured = capture();
    if (captured === null) throw new Error("sync state not written: the account is not bound");
    let failure: { readonly err: unknown } | null = null;
    const run = this.chain.then(async () => {
      try {
        await this.deps.save(account, captured);
      } catch (err) {
        failure = { err };
        this.deps.onError?.(err);
      }
    });
    this.chain = run;
    await run;
    if (failure !== null) throw (failure as { readonly err: unknown }).err;
  }

  /** Drop a queued change without writing it (a removed device). */
  cancel(): void {
    this.stopTimer();
    this.queued = null;
  }

  /** Settles when every write started so far has finished. */
  idle(): Promise<void> {
    return this.chain;
  }

  private stopTimer(): void {
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
  }
}
