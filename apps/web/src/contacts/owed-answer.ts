// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What an accepted contact request still owes once its group stands: the
 * Welcome, then the ack. The entry stays in the commit outbox
 * (`chat/commit-outbox.ts` `OwedAnswer`) until both are done, and is taken up
 * again at the tail of every poll of the group and at launch.
 *
 * The ack goes only after the Welcome landed: the requester closes its `reply`
 * key when the ack arrives. Once the Welcome landed, an ack that nothing can
 * write ends the matter: the requester has the chat, and asking at every poll
 * would change nothing.
 *
 * Every failure is logged with a fixed reason and no identifier.
 *
 * No wasm, no network: the steps are arguments.
 */
export type OwedAnswerResult =
  /** Welcome and ack both went out. */
  | "settled"
  /** Something failed; the entry stays for the retry. */
  | "owed";

export interface SettleAnswerPorts {
  /** Write the Welcome. Throws when it did not land. */
  deliver(): Promise<unknown>;
  /** Record durably that the Welcome landed. */
  noteLanded(): Promise<void>;
  /** Write the ack. Throws when it did not land. */
  ack(): Promise<unknown>;
  /** Whether an ack failure means nothing opens the requester's mailbox. */
  ackRefused(err: unknown): boolean;
  /** Close the entry. */
  done(): Promise<void>;
}

/** Deliver what is owed. `welcomeLanded`: only the ack is. Never throws. */
export async function settleAnswer(
  welcomeLanded: boolean,
  ports: SettleAnswerPorts,
): Promise<OwedAnswerResult> {
  if (!welcomeLanded) {
    try {
      await ports.deliver();
    } catch {
      // eslint-disable-next-line no-console
      console.warn("contact accept: the Welcome did not land; it stays owed");
      return "owed";
    }
    try {
      await ports.noteLanded();
    } catch {
      // Losing the mark costs a duplicate Welcome, which a join ignores.
      // eslint-disable-next-line no-console
      console.warn("contact accept: the landed Welcome could not be recorded");
    }
  }
  try {
    await ports.ack();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("contact accept: the ack did not land");
    if (!ports.ackRefused(err)) return "owed";
  }
  try {
    await ports.done();
  } catch {
    // The entry is re-sent at the next retry; duplicates are ignored.
    // eslint-disable-next-line no-console
    console.warn("contact accept: the owed entry could not be closed");
    return "owed";
  }
  return "settled";
}
