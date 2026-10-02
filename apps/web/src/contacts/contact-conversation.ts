// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which conversation on this device is THE one-to-one with a contact, and how
 * one is founded when there is none.
 *
 * Two screens need it and must agree: Contacts (`Message` on a contact's
 * card) and the call screen's `Invite contacts`
 * (`docs/ux/call-links.md` section 3), which sends the invitation into that
 * conversation and founds it first when the person has never written to the
 * contact from this device.
 *
 * ## The rule
 *
 * 1. The conversation linked on the contact record, when the registry still
 *    holds it. A linked id whose chat is gone (deleted here, or never on this
 *    device) is not an address: seen live 2026-09-26 as a contact whose
 *    Message opened "Conversation not found".
 * 2. Else a conversation whose kind is one-to-one (ADR-0062) and whose PEOPLE
 *    are exactly this account and the contact. People, not leaves: a person
 *    is every one of their devices (ADR-0050), so a roster of three leaves
 *    can be two people, and the contact is known by the account key.
 *
 * A two-person GROUP is never the answer: it is a group that happens to hold
 * two people, and writing an invitation there is not writing to the contact.
 *
 * Pure: the readers are passed in, so vitest (plain node, no wasm handle)
 * can run it.
 */

/** What the rule reads of a conversation. */
export interface ConversationLike {
  readonly groupIdB64: string;
}

/** The contact fields the rule reads. */
export interface ContactLike {
  readonly signaturePubkeyB64: string;
  readonly groupIdB64?: string;
}

export function conversationWithContact<G extends ConversationLike>(input: {
  readonly contact: ContactLike;
  readonly conversations: readonly G[];
  /** This account's person key, base64, or `null` before a session. */
  readonly ownKeyB64: string | null;
  /** The distinct people of a conversation, base64 account keys. A torn
   *  handle reads as empty. */
  readonly peopleOf: (conversation: G) => readonly string[];
  /** Whether the conversation's kind is one-to-one. */
  readonly isOneToOne: (conversation: G) => boolean;
}): G | undefined {
  const { contact, conversations, ownKeyB64 } = input;
  if (contact.groupIdB64 !== undefined) {
    const linked = conversations.find((g) => g.groupIdB64 === contact.groupIdB64);
    if (linked !== undefined) return linked;
  }
  if (ownKeyB64 === null) return undefined;
  for (const g of conversations) {
    if (!input.isOneToOne(g)) continue;
    const people = input.peopleOf(g);
    if (
      people.length === 2 &&
      people.includes(contact.signaturePubkeyB64) &&
      people.includes(ownKeyB64)
    ) {
      return g;
    }
  }
  return undefined;
}
