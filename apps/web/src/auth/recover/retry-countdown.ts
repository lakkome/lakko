// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The live wait of the rate-limited state every recovery form shares
 * (docs/ux/identity-recovery.md section 2, "States every form shares"):
 * "Too many attempts. Try again in 14:59.", counting down from the server's
 * retry time, with the primary button disabled until it reaches zero.
 *
 * The same ticking clock serves the three-word form's own local pause.
 */
import { createSignal, onCleanup, type Accessor } from "solid-js";
import { AuthHttpError } from "../client";

/** The wait a server limit asks for, in seconds, or `null` when `err` is
 *  not an attempts limit with a retry time. `device-request-limited`,
 *  `signup-rate-limited` and `handle-bind-limited` have their own copy and
 *  are not counted down here. */
export function attemptsRetryAfterSecs(err: unknown): number | null {
  if (!(err instanceof AuthHttpError) || err.status !== 429) return null;
  const code = err.errorCode;
  if (code !== null && code !== "too-many-attempts") return null;
  const secs = err.retryAfter;
  return secs !== null && Number.isFinite(secs) && secs > 0 ? secs : null;
}

export interface RetryCountdown {
  /** Milliseconds left; 0 when no wait is running. */
  readonly remainingMs: Accessor<number>;
  /** Whether a wait is running. */
  readonly waiting: Accessor<boolean>;
  /** Start (or replace) the wait: it ends at `untilMs` (unix ms). */
  startUntil(untilMs: number): void;
  /** Start (or replace) the wait: it ends `secs` seconds from now. */
  startFor(secs: number): void;
  stop(): void;
}

/** A once-a-second countdown owned by the calling component. */
export function createRetryCountdown(now: () => number = Date.now): RetryCountdown {
  const [remainingMs, setRemainingMs] = createSignal(0);
  let timer: ReturnType<typeof setInterval> | null = null;
  let until = 0;

  function stop(): void {
    if (timer !== null) clearInterval(timer);
    timer = null;
    setRemainingMs(0);
  }
  function tick(): void {
    const left = until - now();
    if (left <= 0) stop();
    else setRemainingMs(left);
  }
  function startUntil(untilMs: number): void {
    if (timer !== null) clearInterval(timer);
    until = untilMs;
    timer = setInterval(tick, 1000);
    tick();
  }
  onCleanup(() => {
    if (timer !== null) clearInterval(timer);
  });
  return {
    remainingMs,
    waiting: () => remainingMs() > 0,
    startUntil,
    startFor: (secs) => startUntil(now() + secs * 1000),
    stop,
  };
}
