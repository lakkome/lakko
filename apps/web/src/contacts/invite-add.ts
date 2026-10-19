// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order of **Add** on a contact invite page (ADR-0079 section 5, amended
 * 2026-10-19; `docs/ux/contact-invite-link.md` section 3), as one function
 * with the page's steps injected so the order is unit-tested. iOS
 * (`ContactsRepository.addThroughInviteLink`) and Android
 * (`ContactsRepository.addThroughInviteLink`) keep the same order.
 *
 *   1. the ledger's consume: a link used up or gone since the page opened is
 *      dead, and NOTHING is kept: no pending
 *      contact, no held message; the page takes the visitor to Contacts;
 *   2. only then the pending contact is written, before the request is sent
 *      so an answer that comes back at once finds it;
 *   3. the request; a send that fails removes the pending contact again.
 */

/** What the ledger's consume answered. */
export type InviteConsume = "consumed" | "already-consumed" | "exhausted" | "not-found";

export interface InviteAddDeps {
  /** The ledger consume (every link has a ledger row). */
  readonly consume: () => Promise<InviteConsume>;
  /** Write the pending contact; `false` when it was not kept. */
  readonly recordPending: () => Promise<boolean>;
  /** Send the request carrying the link answer. */
  readonly sendRequest: () => Promise<void>;
  /** Remove the pending contact written in step 2. */
  readonly forgetPending: () => Promise<void>;
}

/** How Add ended. A throw from the consume or the send propagates (after the
 *  pending contact was removed again) so the page shows its error line. */
export type InviteAddOutcome = "added" | "dead";

export async function addThroughInviteLink(d: InviteAddDeps): Promise<InviteAddOutcome> {
  const result = await d.consume();
  if (result === "exhausted" || result === "not-found") return "dead";
  const recorded = await d.recordPending();
  try {
    await d.sendRequest();
  } catch (err) {
    if (recorded) await d.forgetPending();
    throw err;
  }
  return "added";
}
