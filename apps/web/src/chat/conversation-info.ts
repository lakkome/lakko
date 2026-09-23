// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the name at the top of a conversation opens
 * (`docs/ux/conversation-info.md`).
 *
 * One entry point, three destinations chosen by what the conversation IS: a
 * one-to-one opens the person, a group opens the group, a channel opens the
 * channel (spec section 1). Before this, one panel did all three jobs with
 * the group-only parts hidden behind a flag, so a chat between two people
 * offered `Group name`, `Create invite link`, `Make admin`, `Transfer
 * ownership` and `Leave group` - none of which mean anything for two people
 * (spec section 0).
 *
 * The rules live here, as plain functions over plain data, because the views
 * that consume them are `.tsx` and `vitest.config.ts` runs a bare `node`
 * environment with no jsdom, no Solid transform and no wasm - nothing inside
 * a component file is reachable from a test. So `GROUP_ONLY_SECTIONS` below
 * is not a restatement of the spec's "Never" line for a reader's benefit: it
 * is what the tests check `personInfoSections` against, under every
 * capability combination, so the defect cannot come back.
 */

import type { ConversationKind } from "./conversation-kind";

/** Which info screen a conversation's header opens. */
export type InfoTarget = "person" | "group" | "channel";

/**
 * One block of an info screen. The three targets share a shape - an
 * identifying header, then the actions, then the detail (spec section 2) -
 * and differ only in which blocks belong to them.
 */
export type InfoSection =
  /* A person. */
  | "person-identity"
  | "person-actions"
  | "safety-number"
  | "contact-name"
  | "remove-contact"
  /* A group. */
  | "group-name"
  | "rename-group"
  | "members"
  | "add-member"
  | "invite-link"
  | "transfer-ownership"
  | "leave-group"
  | "delete-chat"
  /* A channel. */
  | "channel-identity"
  | "manage-channel"
  | "unsubscribe";

/**
 * The blocks a person's info must never hold (spec section 2.1). Each one
 * asks a question two people have no answer to: there is no group to name,
 * no owner to transfer, no admin to make, and nothing to leave that is not
 * simply the other person.
 */
export const GROUP_ONLY_SECTIONS: readonly InfoSection[] = [
  "group-name",
  "rename-group",
  "members",
  "add-member",
  "invite-link",
  "transfer-ownership",
  "leave-group",
];

/**
 * What the header opens for an MLS conversation. A channel is not one of
 * these - it is its own route with its own header - so the mapping here is
 * total over the two kinds the chat store holds.
 */
export function infoTargetForKind(kind: ConversationKind): InfoTarget {
  return kind === "group" ? "group" : "person";
}

/** The accessible name of the header button, which says what it opens. */
export function infoOpenLabel(target: InfoTarget): string {
  switch (target) {
    case "person":
      return "Contact info";
    case "group":
      return "Group info";
    case "channel":
      return "Channel info";
  }
}

/** The heading the opened layer carries. Same words as the button. */
export function infoHeading(target: InfoTarget): string {
  return infoOpenLabel(target);
}

/** What the viewer may do in a group, as the chat already knows it. */
export interface GroupInfoCapabilities {
  /** Owner or admin, and still a member (ADR-0022). */
  readonly canManageMembers: boolean;
  /** Owner or admin, and still a member (ADR-0051). */
  readonly canRename: boolean;
  /** Owner or admin, and still a member (ADR-0028). */
  readonly canMintInvite: boolean;
  /** The owner, and still a member (ADR-0045). */
  readonly isOwner: boolean;
  /** This device's membership is over (ADR-0045 UX 6.2): read-only. */
  readonly membershipEnded: boolean;
}

/** What the viewer is to a channel. */
export interface ChannelInfoCapabilities {
  /** This device holds the publisher keys. */
  readonly isPublisher: boolean;
  /** This device reads the channel as a subscriber. */
  readonly isSubscriber: boolean;
}

/** What a person's info holds (spec section 2.1). */
export interface PersonInfoCapabilities {
  /** The person is in the viewer's contact list, so it can be edited. */
  readonly isContact: boolean;
}

/**
 * The blocks a person's info shows. `Message` is not among them when the
 * info was opened from the conversation itself - the conversation is already
 * open behind the layer - which is the caller's `person-actions` detail
 * rather than a section of its own.
 *
 * Whatever the capabilities, nothing in `GROUP_ONLY_SECTIONS` is ever in
 * the result: that is the whole point of the split.
 */
export function personInfoSections(
  caps: PersonInfoCapabilities,
): readonly InfoSection[] {
  const sections: InfoSection[] = ["person-identity", "person-actions", "safety-number"];
  if (caps.isContact) sections.push("contact-name", "remove-contact");
  return sections;
}

/**
 * The blocks a group's info shows - everything the kind-agnostic panel used
 * to show, under the same capability gates it already used, minus its habit
 * of appearing where there is no group.
 *
 * An ended membership leaves the name and the roster readable and takes
 * every action away: the conversation is over, the record of who was in it
 * is not (spec section 4). What is left in its place is the one thing the
 * reader still can do - close the door behind them.
 *
 * Which of these this module actually GATES, and which it only describes,
 * is worth stating so nobody deletes an entry expecting a control to
 * disappear. `rename-group`, `add-member` and `invite-link` are read by the
 * view as the condition for rendering. `transfer-ownership`, `leave-group`
 * and `delete-chat` are the exit block, whose live control is picked by
 * ADR-0045's own state machine (`exitBlockState` in the view) - they are
 * listed because they are part of what a group's info holds, and because a
 * person's info must be checkable against the whole list.
 */
export function groupInfoSections(
  caps: GroupInfoCapabilities,
): readonly InfoSection[] {
  const sections: InfoSection[] = ["group-name"];
  if (caps.canRename && !caps.membershipEnded) sections.push("rename-group");
  sections.push("members");
  if (caps.membershipEnded) {
    sections.push("delete-chat");
    return sections;
  }
  if (caps.canManageMembers) sections.push("add-member");
  if (caps.canMintInvite) sections.push("invite-link");
  if (caps.isOwner) sections.push("transfer-ownership");
  sections.push("leave-group");
  return sections;
}

/** The blocks a channel's info shows (spec section 2.3). */
export function channelInfoSections(
  caps: ChannelInfoCapabilities,
): readonly InfoSection[] {
  const sections: InfoSection[] = ["channel-identity"];
  if (caps.isPublisher) sections.push("manage-channel");
  // A publisher reads its own channel from its own keys; there is no
  // subscription to end, and offering one would end nothing.
  else if (caps.isSubscriber) sections.push("unsubscribe");
  return sections;
}

/** A contact record as the info layer needs to find one. */
export interface PersonRecordLike {
  readonly signaturePubkeyB64: string;
  readonly groupIdB64?: string;
}

/**
 * The contact record for the other person in a one-to-one chat, or `null`
 * when they are not in the viewer's contact list.
 *
 * Two ways in, and the order matters. The chat's own link (written at accept
 * time, when the contact and the 1x1 group are created together) is exact.
 * The peer's signature key is the fallback that covers the other side of the
 * handshake and a chat joined from a Welcome, where no link was ever
 * written.
 *
 * `null` is a real answer, not a failure: a one-to-one still opens the
 * person, with the two contact-list blocks absent. It never falls back to
 * the group panel, which is the defect this whole module exists to close.
 */
export function findPersonRecord<T extends PersonRecordLike>(
  accepted: readonly T[],
  where: { readonly groupIdB64: string; readonly peerKeyB64: string | null },
): T | null {
  const linked = accepted.find((c) => c.groupIdB64 === where.groupIdB64);
  if (linked !== undefined) return linked;
  if (where.peerKeyB64 === null) return null;
  return accepted.find((c) => c.signaturePubkeyB64 === where.peerKeyB64) ?? null;
}
