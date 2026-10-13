// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 "Phase 2 revised: silent continuity", R.7 and R.8 - what each
 * shared conversation does once a member's account-key reset was ACCEPTED
 * (`contacts/reset-verdict.ts`), as pure decisions.
 *
 * Nothing here has a surface: no copy, no row, no flag (R.0). The chat view
 * and the background receive gather the roster, the role table and this
 * device's role, and act on what these functions answer. Free of wasm, Solid
 * and storage so vitest runs every table. iOS and Android mirror this module
 * (R.12).
 *
 * ## The four cases (R.7)
 *
 *   1. **one-to-one**: not repairable (a one-to-one cannot lose a person).
 *      The surviving side founds a successor by claim-then-build
 *      (`successor-claim.ts`).
 *   2. **repair**: a role-table group the superseded identity is a member or
 *      an admin of. Owners and admins propose `Remove` for the old leaves
 *      and `Add` for the new identity's devices; the owner restores an admin.
 *      The group and the conversation id do not change.
 *   3. **owned**: the superseded identity OWNS the group. Its last leaf
 *      cannot be removed and no transfer can name a new owner; a successor
 *      is founded by claim-then-build.
 *   4. **no-role-table**: nothing is done.
 */
import type { ConversationKind } from "./conversation-kind";

/** This device's ADR-0022 role, as `readPushRotationRole` reports it. */
export type OwnRole = "owner" | "admin" | "member" | null;

/** Which of R.7's cases a group is, for ONE superseded key in its roster. */
export type ResetCase = "one-to-one" | "repair" | "owned" | "no-role-table";

/**
 * | kind  | role table | owner is the superseded key | case            |
 * |-------|------------|-----------------------------|-----------------|
 * | chat  | any        | any                         | `one-to-one`    |
 * | group | absent     | -                           | `no-role-table` |
 * | group | present    | yes                         | `owned`         |
 * | group | present    | no                          | `repair`        |
 */
export function resetCase(input: {
  readonly kind: ConversationKind;
  /** `group.roleTableOwner()`, base64; `undefined` = no role table. */
  readonly ownerKeyB64: string | undefined;
  readonly supersededKeyB64: string;
}): ResetCase {
  if (input.kind === "chat") return "one-to-one";
  if (input.ownerKeyB64 === undefined) return "no-role-table";
  return input.ownerKeyB64 === input.supersededKeyB64 ? "owned" : "repair";
}

/** A roster leaf as these decisions read it: the leaf and the PERSON key
 *  (the account key the leaf is bound to, ADR-0050), both base64. */
export interface PersonLeaf {
  readonly leafIndex: number;
  readonly personKeyB64: string;
}

/** The leaves bound to `supersededKeyB64`: the old identity's devices. */
export function leavesOfKey(roster: readonly PersonLeaf[], supersededKeyB64: string): number[] {
  return roster.filter((m) => m.personKeyB64 === supersededKeyB64).map((m) => m.leafIndex);
}

/**
 * What THIS device proposes in a `repair` group (R.7 case 2), for one
 * superseded key.
 *
 * | own role       | old leaves in roster | new devices missing | proposes                          |
 * |----------------|----------------------|---------------------|-----------------------------------|
 * | owner / admin  | yes                  | any                 | `Remove` of one old leaf per pass |
 * | owner / admin  | any                  | yes                 | `Add` of the missing new devices  |
 * | member / null  | any                  | any                 | nothing (the core refuses it)     |
 *
 * A `Remove` already in the pending set, or this device's own in flight, is
 * not proposed again; a torn roster (`undefined`) proposes nothing. The
 * removal is one leaf per pass because the proposal path takes one target;
 * the next pass (every merged commit re-runs it) takes the next.
 */
export function repairPlan(input: {
  readonly ownRole: OwnRole;
  readonly roster: readonly PersonLeaf[] | undefined;
  readonly supersededKeyB64: string;
  /** Leaves a pending `Remove` proposal already names. */
  readonly pendingRemoveLeaves: readonly number[];
  /** This device's own `Remove` for this key is in flight. */
  readonly removeInFlight: boolean;
  /** This device's membership ended (`membership-ended.ts`). */
  readonly membershipEnded: boolean;
}): { readonly removeLeaf: number | null; readonly mayAdd: boolean } {
  const none = { removeLeaf: null, mayAdd: false };
  if (input.membershipEnded || input.roster === undefined) return none;
  if (input.ownRole !== "owner" && input.ownRole !== "admin") return none;
  const leaves = leavesOfKey(input.roster, input.supersededKeyB64);
  const candidate = leaves.find((l) => !input.pendingRemoveLeaves.includes(l));
  return {
    removeLeaf: input.removeInFlight || candidate === undefined ? null : candidate,
    mayAdd: true,
  };
}

/**
 * R.7 case 2 / the re-admission arm: whether this device proposes an `Add`
 * for the devices of an account whose reset it accepted and whose NEW key
 * is not in the group yet. Only an owner or an admin: a plain member's `Add`
 * for an account that is not a member is refused by the core (ADR-0050's
 * self-service arm does not apply), so it is not sent.
 *
 * An account whose current key IS in the roster is the ordinary ADR-0050
 * re-admission and is not asked here.
 */
export function mayAddResetIdentity(ownRole: OwnRole): boolean {
  return ownRole === "owner" || ownRole === "admin";
}

/**
 * R.7 case 2: the role table the OWNER proposes when the superseded key was
 * an admin: the same table with the new key in its place. `null` when
 * nothing is to be proposed (this device is not the owner, the superseded
 * key is not an admin, the new key is already one, or the new identity is
 * not a member yet: a role for a key no leaf holds is never written).
 */
export function adminRestorePlan(input: {
  readonly ownRole: OwnRole;
  readonly adminKeysB64: readonly string[];
  readonly supersededKeyB64: string;
  readonly newKeyB64: string;
  /** The PERSON keys of the live roster. */
  readonly rosterPersonKeysB64: readonly string[];
}): { readonly adminKeysB64: string[] } | null {
  if (input.ownRole !== "owner") return null;
  if (!input.adminKeysB64.includes(input.supersededKeyB64)) return null;
  if (!input.rosterPersonKeysB64.includes(input.newKeyB64)) return null;
  const next = input.adminKeysB64.filter((k) => k !== input.supersededKeyB64);
  if (!next.includes(input.newKeyB64)) next.push(input.newKeyB64);
  return { adminKeysB64: next };
}

/** One role change the owner proposes next (one per sweep, C-65). */
export type AdminRestoreStep =
  | { readonly kind: "promote"; readonly keyB64: string }
  | { readonly kind: "demote"; readonly keyB64: string };

/**
 * {@link adminRestorePlan} as the NEXT single role change: a role change is
 * one per sweep (C-65), so the swap is two steps on two passes.
 *
 * | superseded key is an admin | new key in roster | new key is an admin | a role change is pending | next step          |
 * |----------------------------|-------------------|---------------------|--------------------------|--------------------|
 * | any                        | any               | any                 | yes                      | none               |
 * | no                         | any               | any                 | no                       | none               |
 * | yes                        | no                | -                   | no                       | none (wait)        |
 * | yes                        | yes               | no                  | no                       | promote the new    |
 * | yes                        | yes               | yes                 | no                       | demote the old     |
 *
 * The new key is promoted BEFORE the old one is demoted, so the person is
 * never without the role in between; nobody but the owner proposes either.
 */
export function adminRestoreStep(input: {
  readonly ownRole: OwnRole;
  readonly adminKeysB64: readonly string[];
  readonly supersededKeyB64: string;
  readonly newKeyB64: string;
  readonly rosterPersonKeysB64: readonly string[];
  readonly roleChangePending: boolean;
}): AdminRestoreStep | null {
  if (input.roleChangePending) return null;
  if (adminRestorePlan(input) === null) {
    // `adminRestorePlan` is null for a non-owner, a superseded key that is
    // no admin, and a new identity that is not a member yet.
    return null;
  }
  return input.adminKeysB64.includes(input.newKeyB64)
    ? { kind: "demote", keyB64: input.supersededKeyB64 }
    : { kind: "promote", keyB64: input.newKeyB64 };
}

/**
 * R.7 case 2 / UX 16.5: the `Members changed` row is suppressed for a commit
 * that leaves the set of PEOPLE unchanged, each person named by account id
 * (the caller maps every person key, alias or current, to the account it
 * knows it under; a key it knows no account for names itself).
 *
 * A torn read on either side suppresses nothing.
 */
export function membersChangedSuppressed(
  peopleBefore: readonly string[] | null,
  peopleAfter: readonly string[] | null,
): boolean {
  if (peopleBefore === null || peopleAfter === null) return false;
  const before = new Set(peopleBefore);
  const after = new Set(peopleAfter);
  if (before.size !== after.size) return false;
  for (const p of before) if (!after.has(p)) return false;
  return true;
}

/** The people of a roster by the name the suppression compares: the account
 *  id a key is known under, else the key itself. */
export function peopleByAccount(
  personKeysB64: readonly string[],
  accountOfKey: (keyB64: string) => string | null,
): string[] {
  const out: string[] = [];
  for (const k of personKeysB64) {
    const person = accountOfKey(k) ?? k;
    if (!out.includes(person)) out.push(person);
  }
  return out;
}

/** How long a person whose superseded devices were removed is expected back
 *  before the memory of the first half is dropped. The journal retention
 *  window; chosen, not measured. */
export const DEVICE_SWAP_PAIR_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

/** The first halves this device saw for one group: account (as
 *  {@link peopleByAccount} names it) -> when its superseded devices left. */
export type AwaitedSwaps = Readonly<Record<string, number>>;

/**
 * R.7 case 2 / UX 16.5 across TWO commits. The repair of a member who reset
 * is a `Remove` of the old leaves and an `Add` of the new devices; when one
 * sweep carries both, {@link deviceSwapOnly} sees no person change. When
 * they land in two commits the first looks like a person leaving and the
 * second like a person joining, though the set of people across the PAIR is
 * unchanged. This judges one commit with the memory of the halves seen
 * before it, and returns the memory to keep.
 *
 * | the commit ...                                                         | row        | memory                 |
 * |------------------------------------------------------------------------|------------|------------------------|
 * | changes no key                                                         | kept       | unchanged              |
 * | changes keys, not people (one batch)                                   | suppressed | unchanged              |
 * | only REMOVES people, each present through superseded keys alone        | suppressed | those people are added |
 * | only ADDS people, each one awaited                                     | suppressed | those people leave it  |
 * | anything else (a person truly gone or new, or a mix with one)          | kept       | whoever came leaves it |
 *
 * "Present through superseded keys alone": every key the roster named for
 * that person before the commit is a key an ACCEPTED reset retired
 * (`isSuperseded`). A person removed while they still hold a current key is
 * a person leaving, and keeps the row. An awaited person older than
 * {@link DEVICE_SWAP_PAIR_WINDOW_MS} is forgotten first. A torn read keeps
 * the row and the memory.
 *
 * The order Add-then-Remove needs no memory: neither commit changes the
 * people.
 */
export function deviceSwapAcrossCommits(input: {
  readonly personKeysBefore: readonly string[] | null;
  readonly personKeysAfter: readonly string[] | null;
  readonly accountOfKey: (keyB64: string) => string | null;
  readonly isSuperseded: (keyB64: string) => boolean;
  readonly awaited: AwaitedSwaps;
  readonly nowMs: number;
}): { readonly suppress: boolean; readonly awaited: AwaitedSwaps } {
  const live: Record<string, number> = {};
  for (const [person, sinceMs] of Object.entries(input.awaited)) {
    if (input.nowMs - sinceMs < DEVICE_SWAP_PAIR_WINDOW_MS) live[person] = sinceMs;
  }
  const before = input.personKeysBefore;
  const after = input.personKeysAfter;
  if (before === null || after === null) return { suppress: false, awaited: live };
  if (membersChangedSuppressed(before, after)) return { suppress: false, awaited: live };
  const name = (k: string): string => input.accountOfKey(k) ?? k;
  const peopleBefore = peopleByAccount(before, input.accountOfKey);
  const peopleAfter = peopleByAccount(after, input.accountOfKey);
  const gone = peopleBefore.filter((p) => !peopleAfter.includes(p));
  const came = peopleAfter.filter((p) => !peopleBefore.includes(p));
  // Whoever is in the roster now is no longer awaited, whatever the verdict.
  for (const p of peopleAfter) delete live[p];
  if (gone.length === 0 && came.length === 0) return { suppress: true, awaited: live };
  const goneBySupersededOnly = gone.every((p) =>
    before.filter((k) => name(k) === p).every((k) => input.isSuperseded(k)),
  );
  const cameAwaited = came.every((p) => input.awaited[p] !== undefined && input.nowMs - (input.awaited[p] as number) < DEVICE_SWAP_PAIR_WINDOW_MS);
  if (came.length === 0 && goneBySupersededOnly) {
    for (const p of gone) live[p] = input.nowMs;
    return { suppress: true, awaited: live };
  }
  if (gone.length === 0 && cameAwaited) return { suppress: true, awaited: live };
  return { suppress: false, awaited: live };
}

// ---------------------------------------------------------------------------
// The two decisions awaiting the maintainer's confirmation. Each is ONE
// function: flipping the default is a one-line change here and nowhere else.
// ---------------------------------------------------------------------------

/**
 * R.7 case 3: after the successor of an OWNED group is built, is ownership
 * transferred back to the identity that reset?
 *
 * `true` (the ADR's default): the founding admin proposes the ownership
 * transfer to the new identity once it is a member, and stays an admin if it
 * was one. `false`: the founding admin stays the owner.
 */
export function restoreOwnershipToResetIdentity(): boolean {
  return true;
}

/**
 * R.7 case 1 / decision D5: does the founder of a successor one-to-one
 * re-send, into it, its own account's messages sent in the OLD one-to-one
 * after the attestation's `signed_at` (they reached only removed devices)?
 *
 * `true` (the ADR's default): re-send. `false`: they are lost with no sign.
 */
export function resendOwnMessagesAfterReset(): boolean {
  return true;
}

/** A stored message of the old one-to-one, as the re-send reads it. */
export interface OldSegmentMessage {
  readonly messageId: number;
  /** Whether THIS account sent it (any of its devices). */
  readonly own: boolean;
  /** Sender-clock or DS time of the row, UNIX milliseconds; `null` when the
   *  row carries no readable time. */
  readonly sentAtMs: number | null;
  /** A chat message with content to send again (not an op, a system row, a
   *  deletion or a call record). */
  readonly resendable: boolean;
}

/**
 * R.7 case 1: the ids of the old segment's messages the founder sends again
 * into the successor, in their original order. Empty when
 * {@link resendOwnMessagesAfterReset} is off. A row with no readable time is
 * NOT re-sent: the rule is "after the reset was signed", and an unknown time
 * proves nothing.
 */
export function messagesToResend(
  messages: readonly OldSegmentMessage[],
  resetSignedAtMs: number,
): number[] {
  if (!resendOwnMessagesAfterReset()) return [];
  return messages
    .filter((m) => m.own && m.resendable && m.sentAtMs !== null && m.sentAtMs > resetSignedAtMs)
    .map((m) => m.messageId)
    .sort((a, b) => a - b);
}

/**
 * R.7 case 3: who the winner adds to an owned group's successor: every
 * person of the old roster except the superseded keys and this device's own
 * account (it is the founder), each once.
 */
export function successorPeople(input: {
  readonly oldRosterPersonKeysB64: readonly string[];
  readonly supersededKeysB64: ReadonlySet<string>;
  readonly ownPersonKeyB64: string;
}): string[] {
  const out: string[] = [];
  for (const k of input.oldRosterPersonKeysB64) {
    if (k === input.ownPersonKeyB64 || input.supersededKeysB64.has(k)) continue;
    if (!out.includes(k)) out.push(k);
  }
  return out;
}

/**
 * R.7 case 3 "the completeness re-check": the people of the old roster that
 * the successor still lacks. A moved owner or admin adds them at the
 * re-admission arm's cadence.
 */
export function successorMissingPeople(input: {
  readonly oldRosterPersonKeysB64: readonly string[];
  readonly successorPersonKeysB64: readonly string[];
  readonly supersededKeysB64: ReadonlySet<string>;
}): string[] {
  const have = new Set(input.successorPersonKeysB64);
  const out: string[] = [];
  for (const k of input.oldRosterPersonKeysB64) {
    if (input.supersededKeysB64.has(k) || have.has(k)) continue;
    if (!out.includes(k)) out.push(k);
  }
  return out;
}

/**
 * R.7 case 3: the admin set the winner restores in the successor: the old
 * table's admins that are members of the successor, minus the superseded
 * keys and minus the successor's owner (an owner is not also listed).
 */
export function successorAdmins(input: {
  readonly oldAdminKeysB64: readonly string[];
  readonly successorPersonKeysB64: readonly string[];
  readonly successorOwnerKeyB64: string;
  readonly supersededKeysB64: ReadonlySet<string>;
}): string[] {
  const members = new Set(input.successorPersonKeysB64);
  return input.oldAdminKeysB64.filter(
    (k) =>
      members.has(k) && k !== input.successorOwnerKeyB64 && !input.supersededKeysB64.has(k),
  );
}

/**
 * R.5 "the old segment after the move" / R.8: a row of an old segment whose
 * MLS-authenticated sender resolves to a SUPERSEDED key is dropped before
 * storage (a removed device still holding the old keys can post there).
 * Only for a key an accepted reset retired; a proven predecessor of a
 * current key is the same person and is never dropped.
 */
export function dropsSupersededSender(
  senderKeyB64: string | undefined,
  supersededKeys: ReadonlyMap<string, string> | ReadonlySet<string>,
): boolean {
  return senderKeyB64 !== undefined && supersededKeys.has(senderKeyB64);
}

/**
 * R.8 "nothing from a superseded sender acts". In a conversation that cannot
 * be repaired in place for that reset (a one-to-one, a group the retired key
 * owned, a group with no role table), or that already moved to a successor,
 * the leaves bound to the retired key are never removed: whoever still holds
 * the old keys can keep posting there. EVERYTHING such a sender posts is
 * dropped there: a chat row, an op and a call offer (those are dropped in
 * every conversation, `dropsSupersededSender`) and also every control or
 * service envelope (a profile, a group name, a pin, a push-routing secret,
 * a committer beacon, a shared-channels list, a scoped-discovery envelope).
 *
 * | sender's key retired by an accepted reset | conversation moved | reset case for that key | inert |
 * |-------------------------------------------|--------------------|-------------------------|-------|
 * | no                                        | -                  | -                       | no    |
 * | yes                                       | yes                | any                     | yes   |
 * | yes                                       | no                 | `repair`                | no    |
 * | yes                                       | no                 | anything else           | yes   |
 *
 * In a group that IS repaired in place the old leaves are evicted by a
 * commit; until it lands their service envelopes are still applied (their
 * chat rows, ops and call signals are not).
 */
export function supersededSenderInert(input: {
  readonly senderKeyB64: string | undefined;
  readonly supersededKeys: ReadonlyMap<string, string> | ReadonlySet<string>;
  readonly kind: ConversationKind;
  readonly ownerKeyB64: string | undefined;
  /** A later segment of the conversation exists (the lineage names one). */
  readonly moved: boolean;
}): boolean {
  const sender = input.senderKeyB64;
  if (sender === undefined || !input.supersededKeys.has(sender)) return false;
  if (input.moved) return true;
  return resetCase({ kind: input.kind, ownerKeyB64: input.ownerKeyB64, supersededKeyB64: sender }) !== "repair";
}

/**
 * R.7 case 1: what is left of a re-send. `ids` is {@link messagesToResend}
 * (ascending); `cursor` is the ORIGINAL id of the last message that landed
 * in the successor (the durable per-conversation cursor of
 * `continuity-store.ts`), or `null` when none has. An interrupted re-send
 * continues after the cursor, never from the first message: what already
 * landed is not sent a second time.
 */
export function resendRemaining(ids: readonly number[], cursor: number | null): number[] {
  return cursor === null ? [...ids] : ids.filter((id) => id > cursor);
}

/** The reply target a re-sent reply carries: "no jump" (the original it
 *  answers lives in the OLD group's journal; its id means nothing in the
 *  successor's). The same value a held reply is flushed with
 *  (`held-send.ts` `REPLY_NO_JUMP_TARGET`). */
export const RESEND_REPLY_TARGET = 0;

/**
 * R.7 case 1: the parts of a stored message a re-send carries beside its
 * text and attachments, so the message is FAITHFUL:
 *
 * | the original was ... | the re-send carries                                        |
 * |----------------------|------------------------------------------------------------|
 * | a reply              | the same quote (author, excerpt), target 0                 |
 * | a forward            | the same attribution and the same origin reference         |
 * | neither              | nothing more                                               |
 */
export function resendParts<
  R extends { readonly target: number },
  F extends object,
>(original: { readonly reply?: R | undefined; readonly forward?: F | undefined }): {
  readonly reply?: R;
  readonly forward?: F;
} {
  return {
    ...(original.reply === undefined
      ? {}
      : { reply: { ...original.reply, target: RESEND_REPLY_TARGET } }),
    ...(original.forward === undefined ? {} : { forward: original.forward }),
  };
}
