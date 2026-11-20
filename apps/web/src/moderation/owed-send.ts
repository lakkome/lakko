// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `reporting-and-moderation.md` section 1.4 - "The block takes effect on
 * this device at once, and on the account's other devices when they sync",
 * and "Block saved, sync failed: Nothing visible. The block holds on this
 * device and syncs later."
 *
 * So the send to the account's other devices never holds the Block button:
 * the register is folded here first, then this runs the send in the
 * background. Each attempt is bounded (a publish that hangs counts as a
 * failure once its time is up); a failed attempt is tried again after the
 * next delay; when the delays run out the send stays owed and the next start
 * sends it (the caller's unsent marker).
 */

/** How long one attempt may take (chosen, not measured). */
export const OWED_SEND_ATTEMPT_MS = 20_000;

/** When a failed send is tried again (chosen, not measured). */
export const OWED_SEND_RETRY_DELAYS_MS: readonly number[] = [30_000, 120_000, 600_000];

export interface OwedSendOptions {
  readonly attemptMs?: number;
  readonly retryDelaysMs?: readonly number[];
  /** A pause, so a test runs the schedule without real time. */
  readonly sleep?: (ms: number) => Promise<void>;
  /** `false` once the work no longer belongs to the signed-in account: the
   *  schedule stops and the send stays owed for that account. */
  readonly isCurrent?: () => boolean;
}

const realSleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

/** One attempt, or `false` when it throws or does not settle within `ms`. */
export async function bounded(attempt: () => Promise<boolean>, ms: number): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeUp = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), ms);
  });
  try {
    return await Promise.race([attempt().catch(() => false), timeUp]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/**
 * Run `attempt(0)`, then `attempt(1)`, ... after each retry delay until one
 * returns `true`. Never throws. Resolves `true` once a send went, `false`
 * when it is still owed. Callers do not await it.
 */
export async function sendOwedInBackground(
  attempt: (index: number) => Promise<boolean>,
  options: OwedSendOptions = {},
): Promise<boolean> {
  const attemptMs = options.attemptMs ?? OWED_SEND_ATTEMPT_MS;
  const delays = options.retryDelaysMs ?? OWED_SEND_RETRY_DELAYS_MS;
  const sleep = options.sleep ?? realSleep;
  const isCurrent = options.isCurrent ?? (() => true);
  for (let index = 0; index <= delays.length; index += 1) {
    if (index > 0) {
      await sleep(delays[index - 1] ?? 0);
    }
    if (!isCurrent()) return false;
    if (await bounded(() => attempt(index), attemptMs)) return true;
  }
  return false;
}
