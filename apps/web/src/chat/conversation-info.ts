// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the name at the top of a conversation opens
 * (`docs/ux/conversation-info.md`).
 *
 * One entry point, three destinations chosen by what the conversation IS: a
 * one-to-one opens the person, a group opens the group, a channel opens the
 * channel (spec section 1).
 *
 * **The rules are no longer decided here.** They are `lakko_ui_rules::info`
 * in `crates/lakko-ui-rules`, reached over wasm here and over UniFFI on iOS
 * and Android. This module used to hold web's own copy, and the three clients
 * held three - one paragraph of one spec, read by three readers who each
 * stopped at a different place. `Remove contact` reached two clients of three,
 * `Unsubscribe` one of three, and `Delete this chat` reached an ended
 * one-to-one only on Android: on web the control sat in the GROUP panel, which
 * a one-to-one had just stopped opening, so ending a one-to-one left a
 * conversation with no way to close it at all.
 *
 * What is still here is everything the shared rule deliberately says nothing
 * about, and could not:
 *
 *   - the WORDS a control carries ({@link infoOpenLabel}, {@link infoHeading});
 *   - the mapping between what this client holds and the small records the
 *     rule reads, so a view hands over plain objects rather than constructing
 *     wasm classes in JSX;
 *   - the narrowing back from the rule's `string[]` to this client's
 *     {@link InfoSection} vocabulary, which is what turns a renamed section in
 *     Rust into a failing test here rather than a control that silently stops
 *     rendering;
 *   - finding the contact record a one-to-one is about ({@link
 *     findPersonRecord}), which is this device's storage and nobody else's
 *     business;
 *   - whether the header opens the group's screen or goes to the contact's own
 *     page ({@link infoOpening}), which is this client's navigation - the
 *     spec says "the contact's own page on web", and a page is a route.
 *
 * Every entry point takes the rules and answers `[]` (or `null`) when they
 * have not loaded. That is deliberate: a caller that finds nothing renders
 * nothing and re-renders when the rules arrive, rather than falling back to a
 * second, drifting copy of the rule - which is the failure this move exists to
 * end. In the running app the rules have long resolved; nothing reaches this
 * module before a session exists, and a session needs wasm.
 */

import type { ConversationKind } from "./conversation-kind";
import type { UiRules } from "./ui-rules";
import { t } from "../l10n";

/** Which info screen a conversation's header opens. */
export type InfoTarget = "person" | "group" | "channel";

/**
 * Every block name the shared rule can hand back, as one list so the type and
 * the runtime check cannot drift apart.
 *
 * These are the kebab-case names `InfoSection::name` emits in
 * `crates/lakko-ui-rules/src/info.rs`. They are a wire vocabulary, not a
 * second rule: nothing here decides whether a block appears.
 */
const SECTION_NAMES = [
  /* A person (spec section 2.1). */
  "message",
  "call",
  "safety-number",
  "rename-contact",
  "remove-contact",
  "add-contact",
  "delete-chat",
  "their-contacts-can-find-me",
  /* A group (spec section 2.2). */
  "group-name",
  "rename-group",
  "members",
  "add-member",
  "invite-link",
  "roles",
  "transfer-ownership",
  "leave-group",
  /* A channel (spec section 2.3). */
  "channel-handle",
  "channel-description",
  "subscriber-count",
  "manage-channel",
  "unsubscribe",
  "channel-members",
] as const;

/**
 * One block of an info screen. The three targets share a shape - an
 * identifying header, then the actions, then the detail (spec section 2) -
 * and differ only in which blocks belong to them.
 */
export type InfoSection = (typeof SECTION_NAMES)[number];

/**
 * The blocks a person's info must never hold (spec section 2.1). Each one
 * asks a question two people have no answer to: there is no group to name,
 * no owner to transfer, no admin to make, and nothing to leave that is not
 * simply the other person. A roster is the same question - a one-to-one has
 * none - and adding a third person starts a new group rather than growing
 * this one.
 *
 * This is an ORACLE, not the rule. The rule is `InfoSection::group_only` in
 * Rust, which is not on the wasm surface; this list is what the tests check
 * `personInfoSections` against, under every capability combination, so the
 * defect cannot come back. Its own drift is caught by the test asserting that
 * every entry is actually reachable from `groupInfoSections`.
 */
export const GROUP_ONLY_SECTIONS: readonly InfoSection[] = [
  "group-name",
  "rename-group",
  "members",
  "add-member",
  "invite-link",
  "roles",
  "transfer-ownership",
  "leave-group",
];

/**
 * What the viewer is to a one-to-one conversation (spec section 2.1).
 *
 * The other person in a one-to-one is a contact by construction
 * (`contacts/adopt-one-to-one.ts`), except a person the viewer removed: the
 * removal stays and the conversation remains, and that card offers
 * `add-contact` in place of rename and remove (amended 2026-10-24).
 */
export interface PersonInfoCapabilities {
  /** This device's membership in the conversation is over (ADR-0045 UX 6.2). */
  readonly membershipEnded: boolean;
  /** The person is on this device's contact list. */
  readonly isContact: boolean;
  /** Contacts of my contacts is on in "Who can find me", and this contact can
   *  be named in the setting (`docs/ux/who-can-find-me.md` section 5.1). Left
   *  out it reads off. */
  readonly contactsOfContactsOn?: boolean;
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
  /**
   * Whether this device can actually leave.
   *
   * ADR-0045's member exit is a `Leave` PROPOSAL, so a group carrying no
   * ADR-0022 role table has no proposal path and there is nothing for the
   * control to do; also false when this device cannot tell which leaf is its
   * own. It is the same condition `ownership-transfer.ts` already hides the
   * exit block on - which is why it is a field: the rule used to offer
   * `leave-group` unconditionally and both this client and Android overrode
   * it in their own view, which is a rule nobody was following.
   */
  readonly canLeave: boolean;
  /** This device's membership is over (ADR-0045 UX 6.2): read-only. */
  readonly membershipEnded: boolean;
}

/** What the viewer is to a channel. */
export interface ChannelInfoCapabilities {
  /** This device holds the publisher keys. */
  readonly isPublisher: boolean;
  /** This device reads the channel as a subscriber. */
  readonly isSubscriber: boolean;
  /** This device has actually counted the subscribers. Only a publisher ever
   *  does, and a reader inventing a number it cannot know is worse than
   *  showing none. */
  readonly subscriberCountKnown: boolean;
}

/**
 * Narrow the rule's `string[]` to the blocks this client knows how to render.
 *
 * A name this client has never heard of is dropped rather than carried into a
 * `.includes` comparison that would silently never match. Dropping is not
 * itself a guard - a renamed block would vanish either way - so the guard is
 * the test, which asserts that nothing is ever dropped for any capability
 * combination on any of the three targets.
 */
function known(names: readonly string[]): readonly InfoSection[] {
  const vocabulary: readonly string[] = SECTION_NAMES;
  return names.filter((n): n is InfoSection => vocabulary.includes(n));
}

/**
 * What the header opens for an MLS conversation. A channel is not one of
 * these - it is its own route with its own header - so the mapping here is
 * total over the two kinds the chat store holds.
 *
 * `null` while the rules are still loading, and also for a destination this
 * client has no screen for: a target nobody can render is better absent than
 * guessed at.
 */
export function infoTargetForKind(
  rules: UiRules | null,
  kind: ConversationKind,
): InfoTarget | null {
  if (rules === null) return null;
  const target = rules.infoTarget(
    kind === "group" ? rules.ConversationKind.Group : rules.ConversationKind.Chat,
  );
  if (target === rules.InfoTarget.Person) return "person";
  if (target === rules.InfoTarget.Group) return "group";
  if (target === rules.InfoTarget.Channel) return "channel";
  return null;
}

/** The accessible name of the header button, which says what it opens. */
export function infoOpenLabel(target: InfoTarget): string {
  switch (target) {
    case "person":
      return t("chat.info.contact");
    case "group":
      return t("chat.info.group");
    case "channel":
      return t("chat.info.channel");
  }
}

/** The heading the opened screen carries. Same words as the button. */
export function infoHeading(target: InfoTarget): string {
  return infoOpenLabel(target);
}

/**
 * The blocks a person's info shows (spec section 2.1), decided by the shared
 * rule.
 *
 * Whatever the capabilities, nothing in {@link GROUP_ONLY_SECTIONS} is ever in
 * the result: that is the whole point of the split, and the rule asserts it in
 * Rust while the tests here assert the boundary carries it.
 *
 * A membership that has ended turns the set over rather than dimming it:
 * `call` goes - the per-call key comes from a group this device is no longer
 * in, so a call control would assert a live session there is not - and
 * `delete-chat` arrives, because closing the door is the one thing still open
 * (ADR-0045 UX 6.2.4).
 */
export function personInfoSections(
  rules: UiRules | null,
  caps: PersonInfoCapabilities,
): readonly InfoSection[] {
  if (rules === null) return [];
  return known(
    rules.personInfoSections(
      new rules.PersonInfoCapabilities(caps.membershipEnded, caps.isContact, caps.contactsOfContactsOn ?? false),
    ),
  );
}

/**
 * How a conversation's header opens its info (`docs/ux/conversation-info.md`
 * section 3).
 *
 * - `contact-page`: a person who is a contact has ONE card, the contact
 *   screen, and the header goes there (`paths.contact`) the way the contact
 *   list does - Message included, which from the chat is just the way back.
 *   Until 2026-09-26 the chat rendered a second copy of the card as a layer
 *   with Message withheld, which needed an "origin" threaded through the
 *   shared rule and showed the same person twice.
 * - `group-screen`: a group's info, a screen pushed from the conversation
 *   with a way back (amended 2026-10-29; until then it was a layer over the
 *   conversation, the one info that did not look like the others).
 * - `nothing`: the destination is not yet known (the shared rules have not
 *   loaded), or it is a person whose contact record cannot be found. The
 *   first: opening a screen now would show whatever branch the rules resolve
 *   to a moment later. The second: every one-to-one peer is a contact by
 *   construction (`contacts/adopt-one-to-one.ts`), so a missing record means
 *   a roster that cannot be read, and there is no "not a contact" card to
 *   draw in its place - the not-a-contact state was retired on 2026-09-26
 *   along with the layer that rendered it.
 */
export type InfoOpening = "contact-page" | "group-screen" | "nothing";

/**
 * The header's opening decision. `personRecord` is only consulted for a
 * person: a group carries a contact record for nobody and still opens its
 * screen.
 */
export function infoOpening(
  target: InfoTarget | null,
  personRecord: PersonRecordLike | null,
): InfoOpening {
  if (target === "person") return personRecord === null ? "nothing" : "contact-page";
  if (target === "group") return "group-screen";
  return "nothing";
}

/**
 * The blocks a group's info shows (spec section 2.2), decided by the shared
 * rule.
 *
 * Which of these this client actually GATES on, and which it only receives, is
 * worth stating so nobody deletes a branch expecting a control to disappear.
 * `rename-group`, `add-member` and `invite-link` are read by the view as the
 * condition for rendering. `transfer-ownership`, `leave-group` and
 * `delete-chat` are the exit block, whose live control is picked by ADR-0045's
 * own state machine (`exitBlockState` in the view) - the rule now agrees with
 * that machine on whether there is an exit at all (`canLeave`), where it used
 * to contradict it and be overridden.
 */
export function groupInfoSections(
  rules: UiRules | null,
  caps: GroupInfoCapabilities,
): readonly InfoSection[] {
  if (rules === null) return [];
  return known(
    rules.groupInfoSections(
      new rules.GroupInfoCapabilities(
        caps.canManageMembers,
        caps.canRename,
        caps.canMintInvite,
        caps.isOwner,
        caps.canLeave,
        caps.membershipEnded,
      ),
    ),
  );
}

/** The blocks a channel's info shows (spec section 2.3), decided by the
 *  shared rule. */
export function channelInfoSections(
  rules: UiRules | null,
  caps: ChannelInfoCapabilities,
): readonly InfoSection[] {
  if (rules === null) return [];
  return known(
    rules.channelInfoSections(
      new rules.ChannelInfoCapabilities(
        caps.isPublisher,
        caps.isSubscriber,
        caps.subscriberCountKnown,
      ),
    ),
  );
}

/** A contact record as the info needs to find one. */
export interface PersonRecordLike {
  readonly signaturePubkeyB64: string;
  readonly groupIdB64?: string;
  /** ADR-0073 R.2: the keys the person held before an accepted reset. */
  readonly previousSignaturePubkeysB64?: readonly string[];
}

/**
 * The contact record for the other person in a one-to-one chat, or `null`
 * when none can be found.
 *
 * Two ways in, and the order matters. The chat's own link (written at accept
 * time, when the contact and the 1x1 group are created together) is exact.
 * The peer's signature key is the fallback that covers the other side of the
 * handshake and a chat joined from a Welcome, where no link was ever
 * written.
 *
 * `null` means a roster that cannot be read, not a person outside the
 * contact list - there is no such person, since every one-to-one peer is
 * adopted into the list (`contacts/adopt-one-to-one.ts`). The header then
 * opens nothing ({@link infoOpening}); it never falls back to the group
 * panel, which is the defect this whole module exists to close.
 *
 * Not part of the shared rule and deliberately so: which record this device
 * happens to hold is this device's storage, not a presentation decision three
 * clients must agree on.
 */
export function findPersonRecord<T extends PersonRecordLike>(
  accepted: readonly T[],
  where: { readonly groupIdB64: string; readonly peerKeyB64: string | null },
): T | null {
  const linked = accepted.find((c) => c.groupIdB64 === where.groupIdB64);
  if (linked !== undefined) return linked;
  if (where.peerKeyB64 === null) return null;
  const peerKeyB64 = where.peerKeyB64;
  return (
    accepted.find((c) => c.signaturePubkeyB64 === peerKeyB64) ??
    // ADR-0073 R.5: an older group of the conversation holds the contact's
    // OLD key; the row was re-keyed at the re-pin and remembers it.
    accepted.find((c) => (c.previousSignaturePubkeysB64 ?? []).includes(peerKeyB64)) ??
    null
  );
}
