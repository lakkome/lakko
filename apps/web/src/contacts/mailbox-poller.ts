// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The identity-mailbox poll, made safe to run from more than one caller at
 * once.
 *
 * `ContactsClient.pollMailbox` is a plain fetch-and-decode: it takes an
 * `after` cursor, returns the decoded events and the new cursor, and has no
 * notion of anybody else polling the same mailbox. That was fine while the
 * only caller was the Contacts route's Refresh button. It is not fine once a
 * background long poll runs for the whole signed-in session beside that
 * button: two overlapping polls from the same cursor would each dispatch the
 * same batch, which means a Welcome joined twice (the second attempt fails
 * because the first consumed the KeyPackage, and surfaces as a bogus error)
 * and a contact request listed twice.
 *
 * The rule here is the one `ChatClient.pollMessages` already follows for group
 * journals:
 *
 *   * the FETCH runs outside any lock — decoding a mailbox touches no MLS
 *     state, and parking a user-initiated refresh behind a 25 s long poll would
 *     make the button useless;
 *   * the DISPATCH runs serialized, one batch at a time, and it re-reads the
 *     cursor first. If the cursor is no longer the `after` this batch was
 *     fetched from, somebody else already consumed those rows, so the batch is
 *     dropped whole (`cursor-moved`) — never re-dispatched, never allowed to
 *     wind the cursor backward;
 *   * the cursor is written only after the dispatch resolves. A dispatch that
 *     throws leaves the cursor where it was (`held`), so the rows are fetched
 *     again rather than skipped — the cursor may lag what was handled, never
 *     lead it (the same direction C-67 pins for group journals).
 */

/** What one poll did with the batch it fetched. */
export type MailboxPollOutcome = "dispatched" | "cursor-moved" | "held";

export interface MailboxPollerDeps<E> {
  readonly readCursor: () => number;
  readonly writeCursor: (cursor: number) => void;
  /** Fetch + decode everything after `after`. Throws on failure. */
  readonly fetch: (
    after: number,
    waitSecs: number,
    signal: AbortSignal | undefined,
  ) => Promise<{ readonly events: readonly E[]; readonly cursor: number }>;
  /** Handle a batch. Runs serialized. A throw holds the cursor. */
  readonly dispatch: (events: readonly E[]) => Promise<void>;
}

export interface MailboxPoller {
  /** Fetch once and dispatch the result under the serialization rule above.
   *  Rejects when the fetch fails or when the dispatch throws (the cursor is
   *  held in that case). */
  poll(waitSecs: number, signal?: AbortSignal): Promise<MailboxPollOutcome>;
}

export function createMailboxPoller<E>(deps: MailboxPollerDeps<E>): MailboxPoller {
  // A promise chain as the dispatch lock: each dispatch runs after the
  // previous one settled, whatever its outcome.
  let tail: Promise<unknown> = Promise.resolve();

  function serialized<T>(fn: () => Promise<T>): Promise<T> {
    const run = tail.then(fn, fn);
    tail = run.catch(() => undefined);
    return run;
  }

  return {
    async poll(waitSecs, signal) {
      const after = deps.readCursor();
      const batch = await deps.fetch(after, waitSecs, signal);
      return serialized(async (): Promise<MailboxPollOutcome> => {
        if (deps.readCursor() !== after) return "cursor-moved";
        if (batch.events.length > 0) {
          try {
            await deps.dispatch(batch.events);
          } catch (err) {
            // Held: the rows will be fetched again. Re-thrown so the caller's
            // error path (and its backoff) sees it.
            throw new MailboxDispatchHeld(err);
          }
        }
        // `batch.cursor` never precedes `after` (the fetch returns rows strictly
        // after it), but guard anyway: a cursor must never move backward.
        if (batch.cursor > after) deps.writeCursor(batch.cursor);
        return "dispatched";
      });
    },
  };
}

/** Thrown by `poll` when the dispatch failed and the cursor was held. */
export class MailboxDispatchHeld extends Error {
  override name = "MailboxDispatchHeld";
  constructor(public readonly cause: unknown) {
    super(
      `mailbox dispatch failed; cursor held: ${
        cause instanceof Error ? cause.message : String(cause)
      }`,
    );
  }
}
