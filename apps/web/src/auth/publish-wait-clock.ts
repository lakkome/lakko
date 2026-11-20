// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The clock of the "Waiting for the directory" state (UX 15.4): while the
 * directory's cool-down runs, the minutes on screen count down, and when it
 * ends the publish is retried by itself, once per wait, while the screen is
 * open. A refusal of that retry records a new wait (or none), which this
 * clock follows. What is shown is `publishOwedView`'s (`key-reset-owed.ts`).
 */
import { createEffect, createSignal, onCleanup, type Accessor } from "solid-js";
import { DiscoveryHttpError } from "../discovery/client";
import { directoryCooldownSecs } from "./key-reset-owed";

export interface PublishWaitClock {
  /** The time the view is computed at; ticks while a wait is recorded. */
  readonly nowMs: Accessor<number>;
}

export function createPublishWaitClock(opts: {
  /** The recorded wait, or `null`. */
  readonly waitUntilMs: Accessor<number | null>;
  /** A retry (automatic or by hand) is out. */
  readonly busy: Accessor<boolean>;
  /** The automatic retry at the end of the wait. */
  readonly retry: () => Promise<void>;
  readonly now?: () => number;
}): PublishWaitClock {
  const now = opts.now ?? Date.now;
  const [nowMs, setNowMs] = createSignal(now());
  let timer: ReturnType<typeof setInterval> | null = null;
  let firedFor: number | null = null;

  function stop(): void {
    if (timer !== null) clearInterval(timer);
    timer = null;
  }
  function tick(): void {
    const t = now();
    setNowMs(t);
    const until = opts.waitUntilMs();
    if (until === null || t < until || opts.busy() || firedFor === until) return;
    firedFor = until;
    void opts.retry().catch(() => {
      // The caller reports it; a new wait (or none) is in the record.
    });
  }

  createEffect(() => {
    const until = opts.waitUntilMs();
    stop();
    if (until === null) return;
    timer = setInterval(tick, 1000);
    tick();
  });
  onCleanup(stop);
  return { nowMs };
}

/** The directory's cool-down refusal (`directoryCooldownSecs`) carried by
 *  `err`, in seconds, or `null`: the waiting copy says it, no error line. */
export function directoryCooldownOf(err: unknown): number | null {
  if (!(err instanceof DiscoveryHttpError)) return null;
  return directoryCooldownSecs({
    status: err.status,
    errorCode: err.errorCode,
    retryAfterSecs: err.retryAfterSecs,
  });
}
