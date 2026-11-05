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
 * A person's chat is a conversation whose kind is one-to-one (ADR-0062) and
 * whose PEOPLE are exactly this account and the contact (under their current
 * key or one they rotated from). People, not leaves: a person is every one of
 * their devices (ADR-0050), so a roster of three leaves can be two people.
 *
 * 1. The conversation linked on the contact record, when the registry holds
 *    it AND it passes that test. A link to a chat that is gone, to a group,
 *    or to a one-to-one with someone else does not count: seen live
 *    2026-11-05 as a contact whose Message opened a three-person group the
 *    row had been linked to while it held two people.
 * 2. Else any conversation that passes the test.
 *
 * A two-person GROUP is never the answer: it is a group that happens to hold
 * two people, and writing there is not writing to the contact.
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
  /** Keys the contact rotated from (ADR-0073 R.5): an older one-to-one holds
   *  the old key and is still their conversation. */
  readonly previousSignaturePubkeysB64?: readonly string[];
}

/** Whether `conversation` is the one-to-one between this account and the
 *  contact. Before a session knows its own key, only the contact's side is
 *  checked. */
export function isOneToOneWithContact<G>(input: {
  readonly conversation: G;
  readonly contact: ContactLike;
  readonly ownKeyB64: string | null;
  readonly peopleOf: (conversation: G) => readonly string[];
  readonly isOneToOne: (conversation: G) => boolean;
}): boolean {
  if (!input.isOneToOne(input.conversation)) return false;
  const people = input.peopleOf(input.conversation);
  if (people.length !== 2) return false;
  const keys = [input.contact.signaturePubkeyB64, ...(input.contact.previousSignaturePubkeysB64 ?? [])];
  const other = input.ownKeyB64 === null ? people : people.filter((p) => p !== input.ownKeyB64);
  if (input.ownKeyB64 !== null && other.length !== 1) return false;
  return other.some((p) => keys.includes(p));
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
  const theirs = (g: G): boolean =>
    isOneToOneWithContact({
      conversation: g,
      contact,
      ownKeyB64,
      peopleOf: input.peopleOf,
      isOneToOne: input.isOneToOne,
    });
  if (contact.groupIdB64 !== undefined) {
    const linked = conversations.find((g) => g.groupIdB64 === contact.groupIdB64);
    if (linked !== undefined && theirs(linked)) return linked;
  }
  // Without the own key only the link can be trusted to name the person's
  // conversation: a scan could match a one-to-one of another account.
  if (ownKeyB64 === null) return undefined;
  return conversations.find(theirs);
}
