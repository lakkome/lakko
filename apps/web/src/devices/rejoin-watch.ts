// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 third amendment - the rejoining device's long poll on its knock's
 * rendezvous, opened BEFORE the knock is posted.
 *
 * The rendezvous is a journal, so a presence note or a Welcome posted before
 * the first read arrived is still returned by it (`after = 0`); opening the
 * read first only saves the time a second read would take. Each row is one
 * answer: a presence note (`isRejoinPresent`) says a device of the account is
 * running; a recovery payload or state bundle row is not an answer and is
 * skipped; anything else is the Welcome.
 *
 * Silence counts only while this device could read: a wait that ends with no
 * answer after a read failed during it rejects instead of returning `null`
 * (a failed read alone does not end a wait: the next read may still bring
 * the answer), so a network
 * failure never reads as "every other device is gone" (which would make this
 * device found the DeviceGroup and cut a running sibling off).
 */
import type { AnswerWatch, RejoinAnswer } from "./device-group-rejoin";

/** One page of the rendezvous. */
export interface RendezvousPage {
  readonly messages: readonly { readonly id: number; readonly payload: Uint8Array }[];
}

/** What the watch reads with. */
export interface RejoinWatchPorts {
  /** One long poll after `after`, at most `waitSecs`. Rejects on failure. */
  fetchPage(after: number, waitSecs: number, signal: AbortSignal): Promise<RendezvousPage>;
  /** `true` for a rejection that means "no rows yet" (a 404 on a fresh
   *  rendezvous), which is not a failure. */
  isEmptyRendezvous(err: unknown): boolean;
  isPresent(row: Uint8Array): boolean;
  /** A row that is neither a presence note nor a Welcome. */
  isOtherRow(row: Uint8Array): boolean;
  now(): number;
  sleep(ms: number, signal: AbortSignal): Promise<void>;
}

/** Server-side long-poll budget per read, in seconds. */
const LONG_POLL_SECS = 15;
/** Pause after a failed read before the next one. Chosen, not measured. */
const RETRY_PAUSE_MS = 1_000;
/** Pause after a "no rows yet" answer that came back at once. */
const EMPTY_PAUSE_MS = 250;

/** Open the watch: the first read starts now. `parent` aborting closes it. */
export function openRejoinWatch(ports: RejoinWatchPorts, parent: AbortSignal): AnswerWatch {
  const abort = new AbortController();
  const onParentAbort = (): void => abort.abort();
  if (parent.aborted) abort.abort();
  else parent.addEventListener("abort", onParentAbort, { once: true });

  const queue: RejoinAnswer[] = [];
  /** Time of the most recent failed read, or `null`. */
  let lastFailureAt: number | null = null;
  let lastFailure: unknown = null;
  let wake: (() => void) | null = null;
  const notify = (): void => {
    const w = wake;
    wake = null;
    w?.();
  };

  void (async () => {
    let cursor = 0;
    while (!abort.signal.aborted) {
      try {
        const page = await ports.fetchPage(cursor, LONG_POLL_SECS, abort.signal);
        for (const m of page.messages) {
          cursor = Math.max(cursor, m.id);
          if (ports.isPresent(m.payload)) queue.push({ kind: "present" });
          else if (!ports.isOtherRow(m.payload)) queue.push({ kind: "welcome", bytes: m.payload });
        }
        if (page.messages.length > 0) notify();
      } catch (err) {
        if (abort.signal.aborted) break;
        if (ports.isEmptyRendezvous(err)) {
          await ports.sleep(EMPTY_PAUSE_MS, abort.signal);
          continue;
        }
        lastFailureAt = ports.now();
        lastFailure = err;
        await ports.sleep(RETRY_PAUSE_MS, abort.signal);
      }
    }
    notify();
  })();

  return {
    async next(deadlineMs: number): Promise<RejoinAnswer | null> {
      const waitStartedAt = ports.now();
      for (;;) {
        const answer = queue.shift();
        if (answer !== undefined) return answer;
        const remaining = deadlineMs - ports.now();
        if (abort.signal.aborted || remaining <= 0) {
          // No answer: silence only if every read of this wait succeeded.
          if (lastFailureAt !== null && lastFailureAt >= waitStartedAt) throw lastFailure;
          return null;
        }
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => {
            if (wake === resolve) wake = null;
            resolve();
          }, remaining);
          wake = (): void => {
            clearTimeout(timer);
            resolve();
          };
        });
      }
    },
    close(): void {
      parent.removeEventListener("abort", onParentAbort);
      abort.abort();
      notify();
    },
  };
}
