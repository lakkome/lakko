// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 amendment (C-88) - the Welcomes the core refused because this
 * device still held their group as active.
 *
 * All a refusal says is that a commit ending this device's membership is ahead
 * of it in the journal, or that the Welcome is a duplicate or a forgery; the
 * core does not report a Welcome's epoch, so no client can tell sooner. The
 * mailbox cursor moved past the row, so `membership-end.ts` keeps the only
 * copy, and one pass does this:
 *
 *   1. Run whatever ends are owed, and try the Welcome.
 *   2. Refused: read that group's journal to its head, let any end the read
 *      set off run, and try again.
 *   3. Refused again with the journal read to its head: the group really is
 *      still active, and the Welcome goes - unless this device recorded a
 *      re-entry request for that group (ADR-0091 section 5): it then stays,
 *      since the open (which tries it as a re-entry first) decides. A read that did not get there
 *      keeps it for the next pass: a refused read proves nothing.
 *
 * Anything but a refusal ends the Welcome's stay, whether it joined or not.
 */

import { heldWelcomeStep, type HeldWelcome } from "./membership-end";

/** What opening a Welcome came to. */
export type HeldWelcomeOutcome<J> =
  | { readonly kind: "joined"; readonly entry: J }
  | { readonly kind: "still-active"; readonly groupIdB64: string }
  | { readonly kind: "not-ours" }
  | { readonly kind: "failed" };

export interface HeldWelcomePort<J> {
  held(): readonly HeldWelcome[];
  open(welcome: HeldWelcome): Promise<HeldWelcomeOutcome<J>>;
  /** Read a journal to its head; `true` when it got there. */
  readJournal(groupIdB64: string): Promise<boolean>;
  /** Run whatever ends are owed. */
  settle(): Promise<void>;
  drop(messageId: number): Promise<void>;
  /** ADR-0091 section 5 - a Welcome whose group this device recorded a
   *  re-entry request for is never dropped: it may be the answer. */
  keepForReentry?(groupIdB64: string): boolean;
  /** A Welcome could not be opened: the person is told, as for any other. */
  noteFailed(): void;
  /** Whether the session this pass was started for still runs. */
  current(): boolean;
}

/** One pass. Resolves with the groups joined; the caller finishes those. */
export async function runHeldWelcomePass<J>(port: HeldWelcomePort<J>): Promise<J[]> {
  const joined: J[] = [];
  if (port.held().length === 0) return joined;
  await port.settle();
  for (const held of [...port.held()]) {
    if (!port.current()) return joined;
    let opened = await port.open(held);
    if (opened.kind === "still-active") {
      const read = await port.readJournal(held.groupIdB64);
      if (!port.current()) return joined;
      await port.settle();
      opened = await port.open(held);
      if (opened.kind === "still-active") {
        if (heldWelcomeStep(read) === "drop" && port.keepForReentry?.(held.groupIdB64) !== true) {
          await port.drop(held.messageId);
        }
        continue;
      }
    }
    if (opened.kind === "joined") joined.push(opened.entry);
    if (opened.kind === "failed") port.noteFailed();
    await port.drop(held.messageId);
  }
  return joined;
}
