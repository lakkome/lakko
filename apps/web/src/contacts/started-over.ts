// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 11 / `docs/ux/identity-recovery.md` 16.11 - what a person
 * sees when a contact STARTED OVER: their account keys were reset in a way
 * this device could not confirm as the same person's (an unsealed reset).
 *
 * Two things are shown, and they are the only exception to the rule that a
 * contact's reset shows nothing (16.1):
 *
 *   - the OLD one-to-one keeps its history and its place; its composer is
 *     replaced by one line and its call buttons are hidden, because the
 *     account's old devices are gone and nothing sent there would arrive;
 *   - a contact request from the NEW keys is an ordinary request card with
 *     one more line under the name.
 *
 * A NEW one-to-one with the same account (the person accepted that request)
 * is a normal chat: "old" is decided by the key the other person's leaves
 * are bound to, never by the account id.
 *
 * Pure: `routes/Chat.tsx`, `routes/Contacts.tsx` and
 * `contacts/MailboxContext.tsx` gather the facts from the contacts store
 * (`loadStartedOverKeys`, `startedOverOf`) and act.
 */
import type { ConversationSurface } from "../people/deleted-surface";

/** Which line stands in place of the composer of a closed one-to-one. */
export type ClosedLine =
  /** ADR-0098: the other person deleted their account. */
  | "deleted"
  /** UX 16.11: the other person started over with a new account. */
  | "started-over";

/** What a conversation offers, and what it says when it is closed. */
export interface ClosableSurface extends ConversationSurface {
  /** The line shown in place of the composer; `null` while it is open. */
  readonly closedLine: ClosedLine | null;
}

const OPEN: ClosableSurface = {
  composer: true,
  callButtons: true,
  reply: true,
  react: true,
  ephemeral: true,
  closedLine: null,
};

function closed(line: ClosedLine): ClosableSurface {
  return { composer: false, callButtons: false, reply: false, react: false, ephemeral: false, closedLine: line };
}

/**
 * Whether this conversation is the OLD one-to-one with a person who started
 * over: a one-to-one whose other person is bound to a key that account left
 * behind by an unsealed reset.
 *
 * `peerKeyB64` is the roster's account key of the one other person, `null`
 * when the roster does not name exactly one (then nothing is decided: a
 * torn roster never closes a chat). A group is never closed by this rule
 * (16.11: groups are not moved and nobody is evicted on this account's
 * behalf).
 */
export function startedOverClosesConversation(input: {
  readonly isOneToOne: boolean;
  readonly peerKeyB64: string | null;
  readonly startedOverKeysB64: ReadonlySet<string> | ReadonlyMap<string, unknown>;
}): boolean {
  if (!input.isOneToOne || input.peerKeyB64 === null) return false;
  return input.startedOverKeysB64.has(input.peerKeyB64);
}

/**
 * What the conversation offers. A deleted account outranks a started-over
 * one (the account behind both is gone for good; its line says so). Either
 * way nothing can be sent, called, replied or reacted to, and typing and
 * read markers stop: none of it would be delivered.
 */
export function closableSurface(input: {
  readonly isOneToOne: boolean;
  readonly peerDeleted: boolean;
  readonly peerStartedOver: boolean;
}): ClosableSurface {
  if (!input.isOneToOne) return OPEN;
  if (input.peerDeleted) return closed("deleted");
  if (input.peerStartedOver) return closed("started-over");
  return OPEN;
}

/**
 * Whether a contact request carries the line "Lakko could not confirm this
 * is the same person as before..." (`contacts.started_over.request_note`):
 * the request is signed by exactly the key the new account of a
 * started-over contact publishes, and the person has not decided about it.
 *
 * `pendingNewKeyB64` is the pending started-over record's key for the
 * request's account id (`startedOverOf(...)?.newKeyB64`), `null` when the
 * account has none (a stranger, an ordinary contact, or one whose new
 * account the person already accepted).
 */
export function requestCarriesStartedOverNote(input: {
  readonly requestKeyB64: string;
  readonly pendingNewKeyB64: string | null;
}): boolean {
  return input.pendingNewKeyB64 !== null && input.pendingNewKeyB64 === input.requestKeyB64;
}

/**
 * UX 16.11 (alignment of 2026-11-27, item D): a request THIS device sent to
 * the new account of someone who started over (added by @name or from a
 * search result) carries the same note on its row under "Sent": the account
 * left keys behind (`startedOverKeysB64` of its pin) and the request went to
 * a key that is not one of them.
 */
export function sentRequestCarriesStartedOverNote(input: {
  readonly requestKeyB64: string | null;
  readonly leftBehindKeysB64: readonly string[];
}): boolean {
  if (input.requestKeyB64 === null || input.leftBehindKeysB64.length === 0) return false;
  return !input.leftBehindKeysB64.includes(input.requestKeyB64);
}

/**
 * UX 16.11 (coordinator follow-up of 2026-11-27): the card of a key an
 * account left behind by an unsealed reset (the old one-to-one's peer)
 * offers no `Add to contacts`: that key's account no longer exists, so a
 * request to it can only fail. The person is added through the new account
 * (its request, or `@name`). Pure.
 */
export function withoutAddForStartedOver<T extends string>(
  sections: readonly T[],
  keyLeftBehind: boolean,
): T[] {
  return keyLeftBehind ? sections.filter((s) => s !== "add-contact") : [...sections];
}
