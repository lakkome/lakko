// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a conversation is called.
 *
 * Extracted from the chat list because it stopped being the chat list's
 * business: `docs/ux/navigation.md` gives the Calls screen rows for the same
 * conversations, and a chat that is "Anna" in one list and "Group" in the
 * other is the drift this codebase keeps paying for. One rule, one place,
 * however many screens name a conversation.
 *
 * ADR-0048 section 5 for a one-to-one, ADR-0051 section 5 for a group: the
 * other person resolved live from the roster so it works however the chat
 * was created, then the shared group name, then a title built from the
 * members - never an id.
 *
 * Pure apart from the catalog word for an unnamed member: every lookup it
 * needs arrives as a parameter, so it can be run by a test (vitest has no
 * jsdom and no Solid transform here) and so neither screen's context leaks
 * into the rule.
 */
import { isUnnamedGroupLabel } from "./group-title";
import { UNNAMED_MEMBER } from "../profile/member-label";

/** What the caller must be able to answer about the conversation. */
export interface ConversationTitleInput {
  /** `"chat"` for a one-to-one, anything else for a group. */
  readonly kind: string;
  /** Person keys of every member, this device included (ADR-0050). */
  readonly memberKeysB64: readonly string[];
  /** This device's own person key, or `null` when it is not resolved. */
  readonly ownKeyB64: string | null;
  /** The group's own label, as the store holds it. */
  readonly fallbackLabel: string;
  /** The shared group name (ADR-0051), when one has arrived. */
  readonly sharedName: string | null | undefined;
  /** A contact's chosen name for this conversation id, when there is one. */
  readonly contactNameById: string | undefined;
  /** A contact's chosen name found by matching members, when there is one. */
  readonly contactNameByMember: string | undefined;
  /** Resolve a person key to a display name. */
  readonly nameOf: (personKeyB64: string) => string;
  /** Build a group title from its name and its members' names. */
  readonly groupTitle: (name: string, memberNames: readonly string[]) => string;
  /** ADR-0087 UX 4.3 — the group's `@name`: the title when there is no name
   *  and no member to describe it by (a solo group with no name). */
  readonly handle?: string | null | undefined;
}

/**
 * The best name for this conversation.
 *
 * The order matters and is the accumulated answer to several bugs, so it is
 * spelled out rather than inlined: a one-to-one is the other person first,
 * because that is who the person thinks they are talking to; a name the user
 * typed for a contact beats a profile they published; and a group with no
 * name is described by who is in it rather than by an identifier nobody
 * recognises.
 */
export function conversationTitle(input: ConversationTitleInput): string {
  const peers = input.memberKeysB64.filter((k) => k !== input.ownKeyB64);
  const isOneToOne = input.kind === "chat";
  const solePeer =
    isOneToOne && input.memberKeysB64.length === 2 && peers.length === 1
      ? peers[0]
      : undefined;
  if (solePeer !== undefined) return input.nameOf(solePeer);
  if (isOneToOne && nonEmpty(input.contactNameById)) return input.contactNameById;
  if (
    isOneToOne &&
    input.memberKeysB64.length <= 2 &&
    nonEmpty(input.contactNameByMember)
  ) {
    return input.contactNameByMember;
  }
  // A member this device cannot name is left out of a group title rather
  // than shown as "Member" (docs/ux/client-redesign.md section 5.1, the iOS
  // rule): "Anna, Ivan" reads as a group, "Anna, Member, Member" does not.
  const unnamed = UNNAMED_MEMBER();
  const names = peers.map(input.nameOf).filter((n) => n.length > 0 && n !== unnamed);
  const label = input.sharedName ?? input.fallbackLabel;
  // UX 4.3 — name, then the member-derived title, then `@name`.
  if (
    !isOneToOne &&
    nonEmpty(input.handle ?? undefined) &&
    names.length === 0 &&
    isUnnamedGroupLabel(label)
  ) {
    return `@${input.handle as string}`;
  }
  return input.groupTitle(label, names);
}

function nonEmpty(text: string | undefined): text is string {
  return text !== undefined && text.length > 0;
}
