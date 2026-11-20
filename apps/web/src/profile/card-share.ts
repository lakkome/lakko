// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 7 / UX `attachments.md` sections 10.1 and 11a - whether a
 * person's contact card may be shared, and what may be forwarded.
 *
 * Outgoing: every profile this account sends carries the flag "my card may
 * be shared", off exactly when the own "who can find me" setting is
 * `Nobody` ({@link ownCardShareable}). The bit is part of the republish
 * signature ({@link withCardShareSignature}), so switching to or from
 * `Nobody` republishes the profile in every one-to-one and group.
 *
 * Incoming: the newest profile held from a person says whether their card
 * may be shared; a person who never sent one counts as shareable. The
 * contact picker shows such a contact disabled ({@link cardPickState}) and a
 * contact-card message describing them is not forwarded
 * ({@link forwardableContentKind}).
 *
 * Also UX `who-can-find-me.md` section 6.2: a group's profile carries the
 * username (and its certificate) only while the setting lets that group's
 * members find the person ({@link usernameForConversation}); a one-to-one
 * always carries it.
 */

/** The core's `VisibilityMode.Nobody` wire value. */
export const VISIBILITY_MODE_NOBODY = 0;

/** Whether the own card may be shared under `setting`. No setting loaded
 *  reads as shareable (the default setting is not `Nobody`). */
export function ownCardShareable(setting: { readonly mode: number } | null): boolean {
  return setting === null || setting.mode !== VISIBILITY_MODE_NOBODY;
}

/** Whether the own card flag FLIPPED between two known settings: the own
 *  profile is then restamped. `null` (no setting yet: signed out, loading)
 *  is never a flip, so loading the setting restamps nothing. */
export function ownCardFlagFlipped(previous: boolean | null, next: boolean | null): boolean {
  return previous !== null && next !== null && previous !== next;
}

/** The republish signature with the card bit: a change of the bit makes
 *  every conversation's profile due again. */
export function withCardShareSignature(signature: string, cardShareable: boolean): string {
  return cardShareable ? signature : `${signature}|nocard`;
}

/** UX `who-can-find-me.md` section 6.2 - the username a profile into a
 *  conversation carries: always in a one-to-one, in a group only while the
 *  setting lets that group's members find the person. */
export function usernameForConversation(
  username: string | null,
  conversation: { readonly oneToOne: boolean; readonly groupAllowed: boolean } | null,
): string | null {
  if (username === null || conversation === null) return username;
  return conversation.oneToOne || conversation.groupAllowed ? username : null;
}

/** How a contact appears in the contact-card picker (UX section 10.1). */
export type CardPickState = "enabled" | "no-username" | "not-shareable";

export function cardPickState(contact: {
  readonly username: string | null;
  readonly cardShareable: boolean;
}): CardPickState {
  if (contact.username === null || contact.username.length === 0) return "no-username";
  return contact.cardShareable ? "enabled" : "not-shareable";
}

/** The content kinds a message can carry (`ChatContent.kind`). */
export type ContentKind = "poll" | "checklist" | "location" | "contactCard" | "article";

/**
 * UX section 11a - whether a message whose content is `kind` may be
 * forwarded. `undefined` = no content (text and files: unchanged). A poll or
 * checklist never; a contact card only when the described person lets their
 * card be shared (`cardShareableOf` of the card's account key).
 */
export function forwardableContentKind(
  content: { readonly kind: ContentKind; readonly accountKeyB64?: string } | undefined,
  cardShareableOf: (accountKeyB64: string) => boolean,
): boolean {
  if (content === undefined) return true;
  switch (content.kind) {
    case "poll":
    case "checklist":
      return false;
    case "contactCard":
      return content.accountKeyB64 === undefined || cardShareableOf(content.accountKeyB64);
    case "location":
    case "article":
      return true;
  }
}
