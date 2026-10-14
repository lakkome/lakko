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
 *
 * ## One poller, many sessions
 *
 * The poller is built once by a provider that outlives a sign-out, and the
 * cursor is a small number: account A and account B can both be at cursor 7.
 * The cursor check above therefore cannot tell "nobody consumed these rows"
 * from "these rows are another account's". So a poll also captures a SCOPE at
 * its start - an opaque value the owner replaces whenever the signed-in
 * mailbox changes - and:
 *
 *   * a batch whose scope is no longer the current one is dropped whole
 *     (`scope-changed`): not dispatched, no cursor written;
 *   * the cursor is read and written THROUGH the captured scope, never through
 *     whatever is current when the write runs, so a dispatch that finished for
 *     A advances A's cursor even if B signed in meanwhile, and never B's.
 */

/** What one poll did with the batch it fetched. */
export type MailboxPollOutcome = "dispatched" | "cursor-moved" | "scope-changed" | "held";

/**
 * `S` is the scope: whatever identifies the mailbox a poll was started for
 * (the signed-in account's inbox). Every callback receives the scope captured
 * when its poll started. A poller with one mailbox for its whole life leaves
 * `scope` out.
 */
export interface MailboxPollerDeps<E, S = undefined> {
  /** The mailbox that is current now. Compared by identity. */
  readonly scope?: () => S;
  readonly readCursor: (scope: S) => number;
  readonly writeCursor: (cursor: number, scope: S) => void;
  /** Fetch + decode everything after `after`. Throws on failure. */
  readonly fetch: (
    after: number,
    waitSecs: number,
    signal: AbortSignal | undefined,
    scope: S,
  ) => Promise<{ readonly events: readonly E[]; readonly cursor: number }>;
  /** Handle a batch. Runs serialized. A throw holds the cursor. */
  readonly dispatch: (events: readonly E[], scope: S) => Promise<void>;
}

export interface MailboxPoller {
  /** Fetch once and dispatch the result under the serialization rule above.
   *  Rejects when the fetch fails or when the dispatch throws (the cursor is
   *  held in that case). */
  poll(waitSecs: number, signal?: AbortSignal): Promise<MailboxPollOutcome>;
}

export function createMailboxPoller<E, S = undefined>(
  deps: MailboxPollerDeps<E, S>,
): MailboxPoller {
  const currentScope = (): S => (deps.scope === undefined ? (undefined as S) : deps.scope());

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
      const scope = currentScope();
      const after = deps.readCursor(scope);
      const batch = await deps.fetch(after, waitSecs, signal, scope);
      return serialized(async (): Promise<MailboxPollOutcome> => {
        // Another mailbox is current (signed out, or another account signed
        // in): these rows are not its rows, whatever its cursor says.
        if (currentScope() !== scope) return "scope-changed";
        if (deps.readCursor(scope) !== after) return "cursor-moved";
        if (batch.events.length > 0) {
          try {
            await deps.dispatch(batch.events, scope);
          } catch (err) {
            // Held: the rows will be fetched again. Re-thrown so the caller's
            // error path (and its backoff) sees it.
            throw new MailboxDispatchHeld(err);
          }
        }
        // `batch.cursor` never precedes `after` (the fetch returns rows strictly
        // after it), but guard anyway: a cursor must never move backward.
        // Through the captured scope: the dispatch above completed for THIS
        // mailbox, so it is this mailbox's cursor that moves.
        if (batch.cursor > after) deps.writeCursor(batch.cursor, scope);
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
