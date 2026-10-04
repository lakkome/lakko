// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 — "a contact reset their account keys", the pure
 * decisions (`docs/ux/identity-recovery.md` section 16).
 *
 * The contact store records the state (`contacts/local-store.ts`
 * `KeyResetRecord`: `previousKey`, the pin before the reset, and
 * `pendingKey`, the published key the KT log confirmed as a reset); this
 * module decides what every shared conversation does with it, and
 * `routes/Chat.tsx` / the background receive render and drive what it
 * decides. Free of wasm, Solid and storage, for the reason
 * `membership-ended.ts` gives: a `.tsx` route cannot be tested, so every
 * rule that could be wrong lives where `vitest` can run it.
 *
 * ## The three branches per shared group (UX 16.3 / 16.4 / 16.5)
 *
 *   * **one-to-one** — the chat with the old keys CLOSES (ADR-0062: a
 *     one-to-one cannot lose a person, so it ends rather than being
 *     repaired); nothing is proposed, nothing is ever sent into it again.
 *   * **owned-by-reset** — the old identity OWNS the group: `owner_survives`
 *     (C-63) refuses every commit that would remove it and no transfer can
 *     name a new owner (the owner's key is gone). Nothing is proposed; an
 *     admin recreates the group (16.5).
 *   * **evict** — a role-table group the old identity is a plain member or
 *     admin of, and THIS device's account is owner or admin: the removal
 *     of every leaf resolving to `previousKey` is proposed, silently.
 *   * **flag-only** — everything else: a plain member (the core refuses a
 *     member's proposal to remove another account) or a group with no role
 *     table (no proposal path). The contact is flagged on the row and
 *     nothing happens.
 *
 * The branch is a function of the group's KIND, its role table and THIS
 * device's role, never of anything the contact asserted.
 */
import { t } from "../l10n";
import type { ConversationKind } from "./conversation-kind";
import { personKeyOf, type RosterLeaf } from "./person-key";
import { base64Encode } from "../auth/account-id";

/** This device's ADR-0022 role, as `readPushRotationRole` reports it. */
export type OwnRole = "owner" | "admin" | "member" | null;

/** What a shared group does about a flagged contact. */
export type ResetGroupBranch = "one-to-one" | "owned-by-reset" | "evict" | "flag-only";

/**
 * Decide the branch (UX 16.3-16.5, the brief's "three branches per shared
 * group").
 *
 * | kind | role table | owner is `previousKey` | own role      | branch           |
 * |------|------------|------------------------|---------------|------------------|
 * | chat | any        | any                    | any           | `one-to-one`     |
 * | group| absent     | -                      | any           | `flag-only`      |
 * | group| present    | yes                    | any           | `owned-by-reset` |
 * | group| present    | no                     | owner / admin | `evict`          |
 * | group| present    | no                     | member / null | `flag-only`      |
 *
 * A `null` role (the table could not be read) is a plain member: a torn
 * read never proposes anything.
 */
export function resetGroupBranch(input: {
  readonly kind: ConversationKind;
  /** `group.roleTableOwner()`, base64; `undefined` = no role table. */
  readonly ownerKeyB64: string | undefined;
  readonly previousKeyB64: string;
  readonly ownRole: OwnRole;
}): ResetGroupBranch {
  if (input.kind === "chat") return "one-to-one";
  if (input.ownerKeyB64 === undefined) return "flag-only";
  if (input.ownerKeyB64 === input.previousKeyB64) return "owned-by-reset";
  if (input.ownRole === "owner" || input.ownRole === "admin") return "evict";
  return "flag-only";
}

/** The leaves of `roster` whose PERSON key (the account key, ADR-0050) is
 *  `previousKeyB64`: the old identity's devices. */
export function leavesOfPreviousKey(
  roster: readonly RosterLeaf[],
  previousKeyB64: string,
): number[] {
  return roster
    .filter((m) => base64Encode(personKeyOf(m)) === previousKeyB64)
    .map((m) => m.leafIndex);
}

/**
 * The `applied` test of `proposal-based-membership.md` section 4 for the
 * eviction: the old key is ABSENT from the roster. Compared on the person
 * key, never a leaf index (OpenMLS reuses a vacated leaf for the next Add).
 * A torn roster (`undefined`) is never "applied": a device that cannot
 * read the roster cannot claim the removal happened (invariant #4 as the
 * ADR states it for this case).
 */
export function evictionApplied(
  roster: readonly RosterLeaf[] | undefined,
  previousKeyB64: string,
): boolean {
  if (roster === undefined) return false;
  return leavesOfPreviousKey(roster, previousKeyB64).length === 0;
}

/** Whether THIS device should propose the eviction now (UX 16.3). */
export function shouldProposeEviction(input: {
  readonly branch: ResetGroupBranch;
  /** `evictionApplied` over the live roster. */
  readonly applied: boolean;
  /** A removal of `previousKey` is already in the live pending set, or
   *  this device's own request for it is in flight. */
  readonly alreadyPending: boolean;
  /** This device's membership ended (`membership-ended.ts`). */
  readonly membershipEnded: boolean;
}): boolean {
  if (input.membershipEnded) return false;
  if (input.branch !== "evict") return false;
  if (input.applied) return false;
  return !input.alreadyPending;
}

/** The secondary line of the flagged contact's members-panel row. */
export type MemberRowResetFlag = "being-removed" | "flag";

/**
 * The row annotation (UX 16.3 / 16.5): `Being removed.` for the owner and
 * the admins who proposed it, in a group where it IS proposed;
 * `Account keys reset.` for a plain member of that group and for everyone
 * in the owned group and in a table-less group. `null` once the old key
 * has left the roster (the row is gone).
 */
export function memberRowResetFlag(input: {
  readonly branch: ResetGroupBranch;
  readonly applied: boolean;
}): MemberRowResetFlag | null {
  if (input.applied) return null;
  return input.branch === "evict" ? "being-removed" : "flag";
}

/** What is blocked while the contact is `reset-unverified` (UX 16.1). */
export interface BlockedActions {
  readonly startChat: boolean;
  readonly addToGroup: boolean;
  readonly approveJoin: boolean;
  readonly includeInRecreate: boolean;
  readonly call: boolean;
}

/** The 16.1 table. Nothing is blocked for a `normal` contact; everything
 *  listed is blocked for a flagged one. Nothing is ever blocked for anyone
 *  else. */
export function blockedActions(flagged: boolean): BlockedActions {
  return {
    startChat: flagged,
    addToGroup: flagged,
    approveJoin: flagged,
    includeInRecreate: flagged,
    call: flagged,
  };
}

/**
 * UX 16.8 — the ADR-0050 scheduled device-list arm's ADD branch is
 * SUPPRESSED for an account whose contact is `reset-unverified` or whose
 * published key is not the pin. Without this, every member's client would
 * propose adding the new identity's devices to every shared group within
 * ten minutes of the reset, ahead of any verification. Only an account whose
 * published key IS the pin (`current`) is looked up for devices to admit; a
 * `reset` (flagged now), a `changed` (the gate's own refusal), an `absent`
 * directory row and a failed check (`null`) all leave the account alone.
 * The RETIRE side is this account's own and is not touched here.
 */
export function readmissionAddEligible(
  flagged: boolean,
  check: "current" | "reset" | "changed" | "absent" | null,
): boolean {
  if (flagged) return false;
  return check === "current";
}

/**
 * The bubble warning (UX 16.1): a message whose MLS-authenticated sender
 * resolves to a flagged contact's `pendingKey` carries, under the name,
 * `Not verified since {name} reset their account keys.`. Never for a
 * verified contact (the record is gone), never for a stranger (no record).
 * Returns the contact's row key, so the caller names the person through
 * the account id, or `null`.
 */
export function bubbleWarningFor(
  senderKeyB64: string | undefined,
  /** `pendingKeyB64` → the contact's row key, for every flagged contact. */
  pendingKeys: ReadonlyMap<string, string>,
): string | null {
  if (senderKeyB64 === undefined) return null;
  return pendingKeys.get(senderKeyB64) ?? null;
}

/**
 * UX 16.4: a late message into a CLOSED one-to-one is dropped before it is
 * stored. Only the application (chat) rows are messages; a commit from the
 * person's own other devices (ADR-0050) still applies, and a service
 * envelope was never stored anyway.
 */
export function dropsLateMessage(closed: boolean, eventKind: string): boolean {
  return closed && (eventKind === "application" || eventKind === "op");
}

// ---------------------------------------------------------------------------
// Copy (UX 16.7) — one definition each, so the component cannot drift
// ---------------------------------------------------------------------------

export const KEYS_RESET_ROW_SUB = (): string => t("contacts.keys_reset.row_sub");
export const KEYS_RESET_BANNER = (name: string | null): string =>
  name === null
    ? t("contacts.keys_reset.banner_unnamed")
    : t("contacts.keys_reset.banner", { name });
export const KEYS_RESET_BANNER_ACTION = (): string => t("contacts.keys_reset.banner_action");
export const KEYS_RESET_VERIFY_FIRST = (name: string | null): string =>
  name === null
    ? t("contacts.keys_reset.verify_first_short")
    : t("contacts.keys_reset.verify_first", { name });
export const KEYS_RESET_VERIFY_FIRST_SHORT = (): string => t("contacts.keys_reset.verify_first_short");
export const KEYS_RESET_OPEN_CONTACT = (): string => t("contacts.keys_reset.open_contact");
export const KEYS_RESET_PICKER_SUB = (): string => t("contacts.keys_reset.picker_sub");

export const BUBBLE_WARNING = (name: string): string =>
  t("chat.keys_reset.bubble_warning", { name });
export const MEMBER_BEING_REMOVED = (): string => t("chat.keys_reset.member_being_removed");
export const MEMBER_FLAG = (): string => t("chat.keys_reset.member_flag");
export const SYSTEM_REMOVED = (name: string): string =>
  t("chat.keys_reset.system_removed", { name });
export const ONE_TO_ONE_CLOSED = (name: string): string =>
  t("chat.keys_reset.one_to_one_closed", { name });
export const ONE_TO_ONE_ROW = (name: string): string =>
  t("chat.keys_reset.one_to_one_row", { name });
export const ONE_TO_ONE_COMPOSER = (): string => t("chat.keys_reset.one_to_one_composer");
export const START_NEW_CHAT = (): string => t("chat.keys_reset.start_new_chat");
export const OWNED_NOTICE = (name: string): string =>
  t("chat.keys_reset.owned_notice", { name });
export const OWNED_NOTICE_MEMBER = (): string => t("chat.keys_reset.owned_notice_member");

/** The members-panel row line for a flag, or `null`. */
export function memberRowResetLine(flag: MemberRowResetFlag | null): string | null {
  if (flag === null) return null;
  return flag === "being-removed" ? MEMBER_BEING_REMOVED() : MEMBER_FLAG();
}
