// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A cancellable long-poll driver — the scheduling half of every automatic
 * receive loop on this client, kept free of DOM, wasm and network so the
 * scheduling rules can be tested on their own.
 *
 * Before this module the web client had no automatic receive at all: a chat
 * fetched its journal on mount and on user actions, and the identity mailbox
 * only when the user pressed Refresh. Messages, Welcomes and 1x1 call
 * signaling therefore sat on the Delivery Service until somebody clicked.
 * The DS already supports long polling (`?wait=<secs>`, capped at 60 on both
 * journal kinds), so the fix is a loop that parks one request at a time and
 * re-arms the moment it returns.
 *
 * ## What the loop owns, and what it does not
 *
 * It owns WHEN to run the next round trip: immediately after a successful
 * one, after a growing backoff after a failed one, never faster than a small
 * floor, not at all while paused, and never again once aborted or told to
 * stop. It does NOT own what a round trip does — that is `tick`, supplied by
 * the caller — and it does not own correctness under overlap. Every caller
 * already serializes its apply half behind its own lock and re-reads its
 * cursor there (see `ChatClient.pollMessages` and `contacts/mailbox-poller.ts`),
 * so a tick that races a user-initiated refresh is dropped by the caller, not
 * prevented here.
 */

/** Long-poll budget handed to the DS. Below the server's 60 s cap, and below
 *  the 30 s worst-case poll the ADR-0045 beacon freshness window is sized
 *  against, so a web observer meets the same constraint the natives do. */
export const LONG_POLL_WAIT_SECS = 25;

/** First backoff after a transport-level failure (the DS unreachable). */
export const NETWORK_BACKOFF_MS = 2_000;
/** First backoff after any other failure (an HTTP error, a decode error). */
export const OTHER_BACKOFF_MS = 5_000;
/** Ceiling for the doubling backoff, so a long outage costs one request per
 *  half minute rather than a request per base interval. */
export const MAX_BACKOFF_MS = 30_000;
/**
 * The shortest spacing between the START of two successful ticks.
 *
 * A healthy long poll never trips it — a parked request takes seconds. It
 * exists for the pathological case where every round trip returns at once
 * (a proxy that strips `wait`, a journal that keeps handing back the same
 * batch because a persist is being held) so a bug elsewhere degrades into a
 * slow poll instead of a tight loop hammering the server.
 */
export const MIN_TICK_SPACING_MS = 500;

/** How the caller classifies a failed tick. */
export type PollFailureKind = "network" | "other";

export interface LongPollLoopDeps {
  /** `false` ends the loop for good (for example: this device's membership of
   *  the group has ended). Consulted before every iteration. */
  readonly shouldContinue: () => boolean;
  /** `true` parks the loop in `waitUntilResumed` instead of ticking. */
  readonly isPaused: () => boolean;
  /** Resolves when the pause condition may have changed, or when `signal`
   *  aborts. Resolving while still paused is harmless — the loop re-checks. */
  readonly waitUntilResumed: (signal: AbortSignal) => Promise<void>;
  /** One round trip. Resolves with an extra delay in ms to wait before the
   *  next tick (0 for "go again now"); throws on failure. */
  readonly tick: (signal: AbortSignal) => Promise<number>;
  readonly classifyFailure: (err: unknown) => PollFailureKind;
  /** Cancellable sleep; must resolve (not reject) when `signal` aborts. */
  readonly sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  /** Monotonic-enough clock in ms, for the tick-spacing floor. */
  readonly now: () => number;
  /** Observes every failure with the backoff chosen for it. */
  readonly onFailure?: (err: unknown, backoffMs: number) => void;
}

/**
 * The backoff after the `consecutiveFailures`-th failure in a row: the base
 * for the failure kind, doubled per further consecutive failure, capped at
 * {@link MAX_BACKOFF_MS}. `consecutiveFailures` below 1 is treated as 1.
 */
export function backoffFor(
  kind: PollFailureKind,
  consecutiveFailures: number,
): number {
  const base = kind === "network" ? NETWORK_BACKOFF_MS : OTHER_BACKOFF_MS;
  const doublings = Math.min(Math.max(consecutiveFailures, 1) - 1, 16);
  return Math.min(MAX_BACKOFF_MS, base * 2 ** doublings);
}

/**
 * Run the loop until `signal` aborts or `shouldContinue` turns false.
 * Never rejects: a failed tick is reported through `onFailure` and backed
 * off, because a single bad iteration must not end the receive loop.
 */
export async function runLongPollLoop(
  signal: AbortSignal,
  deps: LongPollLoopDeps,
): Promise<void> {
  let consecutiveFailures = 0;
  while (!signal.aborted && deps.shouldContinue()) {
    if (deps.isPaused()) {
      await deps.waitUntilResumed(signal);
      continue;
    }
    const startedAt = deps.now();
    let delayMs: number;
    try {
      delayMs = Math.max(0, await deps.tick(signal));
      consecutiveFailures = 0;
      const elapsed = deps.now() - startedAt;
      if (elapsed < MIN_TICK_SPACING_MS) {
        delayMs = Math.max(delayMs, MIN_TICK_SPACING_MS - elapsed);
      }
    } catch (err) {
      if (signal.aborted) return;
      consecutiveFailures += 1;
      delayMs = backoffFor(deps.classifyFailure(err), consecutiveFailures);
      deps.onFailure?.(err, delayMs);
    }
    if (signal.aborted) return;
    if (delayMs > 0) await deps.sleep(delayMs, signal);
  }
}

/** A `setTimeout` sleep that resolves early (never rejects) on abort. */
export function sleepCancellable(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    const onAbort = (): void => {
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/**
 * A resumable latch for `waitUntilResumed`: `wait(signal)` parks until the
 * next `wake()` or until `signal` aborts. Every waiter parked at the moment of
 * a `wake()` is released.
 */
export function createWakeLatch(): {
  wait: (signal: AbortSignal) => Promise<void>;
  wake: () => void;
} {
  let waiters: Array<() => void> = [];
  return {
    wait(signal) {
      return new Promise((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        const release = (): void => {
          signal.removeEventListener("abort", release);
          waiters = waiters.filter((w) => w !== release);
          resolve();
        };
        waiters.push(release);
        signal.addEventListener("abort", release, { once: true });
      });
    },
    wake() {
      const current = waiters;
      waiters = [];
      for (const w of current) w();
    },
  };
}
