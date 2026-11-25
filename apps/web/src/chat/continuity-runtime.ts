// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 "Phase 2 revised", R.6 and R.7 cases 1 and 3 - the ORDER in
 * which a device continues a conversation that cannot be repaired in place,
 * as one function over an injected port.
 *
 * `successor-claim.ts` holds the rules (who is eligible, which claim wins)
 * and `reset-repair.ts` the plans (who is added, which roles are restored).
 * This module is the sequence between them, and the sequence is the point:
 *
 *   1. create the successor locally and claim its DS journal; nothing is
 *      sent to anyone;
 *   2. post the claim into the OLD group and note its DS message id;
 *   3. wait until the old journal has been read up to that id; an EARLIER
 *      eligible claim means this device lost: discard, adopt the earlier one;
 *   4. only the winner builds: it delivers the Welcomes it prepared, records
 *      the lineage, carries the name, adds the remaining people, restores
 *      the roles and (decision) the owner, and re-sends what was sent into
 *      the dead session (decision).
 *
 * The conversation moves to the successor (the lineage, the held
 * successor) only AFTER its Welcomes were delivered. Moved before, it is a
 * group only this account is in: the row is drawn from a roster naming a
 * key that never sent a profile, and every message sent there is read by
 * nobody (seen live 2026-11-25, when the reset account's new mailbox could
 * not be reached). And nothing is prepared or claimed while the new
 * identity cannot be reached at all ({@link ContinuityPort.newIdentityReachable}):
 * the old one-to-one stays the conversation, and a send into it is held
 * (`held-send.ts`).
 *
 * "Nothing is sent before the claim is confirmed" is what keeps a lost
 * claim harmless; a lost claim AFTER Welcomes went out would leave people in
 * a group nobody continues in (R.14), and this order is the only thing that
 * prevents it. So it lives here, behind a port a test can drive, rather
 * than inside a provider no test can mount.
 *
 * Every step is idempotent and the pass is re-entered from the top at each
 * trigger (a poll tail, an accepted reset, a start): a crash at any point
 * resumes from the claim state the store holds.
 */
import type { ConversationKind } from "./conversation-kind";
import type { StoredGroupClosed } from "./group-closed-store";
import {
  restoreOwnershipToResetIdentity,
  successorAdmins,
  successorMissingPeople,
} from "./reset-repair";
import {
  claimOutcome,
  shouldFoundSuccessor,
  unrepairableBy,
  type SuccessorClaim,
} from "./successor-claim";

/** What the pass reads off one group, live. */
export interface ContinuityFacts {
  readonly kind: ConversationKind;
  /** The PERSON keys of the roster, base64. */
  readonly rosterPersonKeysB64: readonly string[];
  /** `undefined` = no role table. */
  readonly ownerKeyB64: string | undefined;
  readonly adminKeysB64: readonly string[];
  /** This device's membership is live. */
  readonly active: boolean;
}

/** Everything the pass does to the world. */
export interface ContinuityPort {
  ownPersonKeyB64(): string | null;
  /** Every key an accepted reset retired -> its account id (base64). */
  supersededKeys(): ReadonlyMap<string, string>;
  /** The key this device now trusts for an account, or `null`. */
  currentKeyOf(accountIdB64: string): string | null;
  /** `undefined` when the group is not held or cannot be read. */
  facts(groupIdB64: string): ContinuityFacts | undefined;
  claimState(oldGroupIdB64: string): StoredGroupClosed;
  /** How far this device has read the old group's journal. */
  cursorOf(oldGroupIdB64: string): number | undefined;
  /** Whether the new identity publishes at least one device (R.8). */
  newIdentityHasDevices(accountIdB64: string): Promise<boolean>;
  /**
   * Whether this device holds anything that lets it write a Welcome to the
   * new identity's mailbox (a kept grant of the new key, a delegation). A
   * reset account knows nobody, and since ADR-0078 the continuity key
   * reaches no mailbox, so this is usually `false` until the person sends
   * this account a contact request from the new key (UX 16.6).
   */
  newIdentityReachable(accountIdB64: string): Promise<boolean>;
  /**
   * Step 1: create the successor of `oldGroupIdB64` locally, add the new
   * identity's devices and this account's other devices in ONE commit,
   * claim its DS journal, and keep the Welcome for step 4. Sends nothing.
   * Returns the successor's id.
   */
  prepareSuccessor(oldGroupIdB64: string, newIdentityAccountIdB64: string): Promise<string>;
  /** Step 2: post the claim; resolves the DS message id. The own-claim
   *  record is written before the publish. */
  postClaim(oldGroupIdB64: string, successorGroupIdB64: string): Promise<number>;
  /** Forget an own claim whose publish never produced a row. */
  abandonOwnClaim(oldGroupIdB64: string): Promise<void>;
  /** A lost claim: drop the local successor (never announced). */
  discardSuccessor(successorGroupIdB64: string): Promise<void>;
  /** Step 3 decided: record the conversation's successor, clear the own
   *  claim. */
  settle(oldGroupIdB64: string, winner: SuccessorClaim): Promise<void>;
  /** The lineage register `successor -> conversation of the old group`.
   *  Idempotent (write-once). */
  recordLineage(oldGroupIdB64: string, successorGroupIdB64: string): Promise<void>;
  /** Step 4a: deliver the Welcomes, THEN make the successor a held
   *  conversation (never the other way round: see the module docs).
   *  Idempotent. `unreachable`: the new identity's mailbox could not be
   *  written, nothing was made visible; `not-ready`: the successor could not
   *  be brought up. The next pass tries again either way. */
  deliverSuccessor(oldGroupIdB64: string, successorGroupIdB64: string): Promise<SuccessorDelivery>;
  /** Step 4b: re-send the group name into the successor with its ORIGINAL
   *  stamp (ADR-0051). Idempotent. */
  carryName(oldGroupIdB64: string, successorGroupIdB64: string): Promise<void>;
  /** Step 4c: add one person of the old roster (by the key the old roster
   *  names) to the successor. Resolves `false` when they could not be
   *  resolved or reached; the completeness re-check retries. */
  addPerson(successorGroupIdB64: string, personKeyB64: string): Promise<boolean>;
  /** Whether the completeness re-check of `successorGroupIdB64` is due (the
   *  re-admission arm's interval, per successor); stamps it when so. */
  recheckDue(successorGroupIdB64: string): boolean;
  /** Step 4d: set the successor's admin set (one role change). */
  setAdmins(successorGroupIdB64: string, adminKeysB64: readonly string[]): Promise<void>;
  /** Step 4e: propose the ownership transfer to the new identity, the
   *  founder staying an admin when `stayAdmin`. */
  transferOwnership(
    successorGroupIdB64: string,
    newOwnerKeyB64: string,
    stayAdmin: boolean,
  ): Promise<void>;
  /** R.7 case 1 / D5: send again, into the successor, this account's
   *  messages sent in the old one-to-one after `signedAtMs`. Idempotent
   *  (records that it ran). */
  resendOwnMessages(
    oldGroupIdB64: string,
    successorGroupIdB64: string,
    accountIdB64: string,
  ): Promise<void>;
}

/** How step 4a ended. */
export type SuccessorDelivery = "delivered" | "unreachable" | "not-ready";

/** What one pass did, for the caller's log and the tests. */
export type ContinuityStep =
  /** Nothing to continue (repairable, no accepted reset, not held). */
  | "nothing"
  /** Another device's claim is the conversation's successor: adopted. */
  | "adopted"
  /** The new identity publishes no device yet (R.8): retry later. */
  | "waiting-for-devices"
  /** Nothing this device holds reaches the new identity's mailbox: no
   *  successor is prepared or moved to, retry later. */
  | "waiting-for-reach"
  /** This device is not eligible to found. */
  | "not-eligible"
  /** Steps 1 and 2 ran: the claim is posted. */
  | "claimed"
  /** The old journal has not been read up to the own claim yet. */
  | "waiting-for-journal"
  /** An earlier eligible claim won: the local successor was discarded. */
  | "lost"
  /** This device won and built (or continued building) the successor. */
  | "built"
  /** A moved admin or owner that did not found the successor added people
   *  the successor still lacked. */
  | "rechecked"
  /** A step failed; the next pass resumes. */
  | "failed";

/**
 * One pass over one OLD group. Never throws.
 */
export async function continueConversation(
  port: ContinuityPort,
  oldGroupIdB64: string,
): Promise<ContinuityStep> {
  try {
    return await run(port, oldGroupIdB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[continuity] pass failed; resumed at the next trigger", err);
    return "failed";
  }
}

async function run(port: ContinuityPort, oldGroupIdB64: string): Promise<ContinuityStep> {
  const facts = port.facts(oldGroupIdB64);
  if (facts === undefined) return "nothing";
  const own = port.ownPersonKeyB64();
  const superseded = port.supersededKeys();
  if (superseded.size === 0) return "nothing";
  const unrepairableKeyB64 = unrepairableBy({
    kind: facts.kind,
    rosterPersonKeysB64: facts.rosterPersonKeysB64,
    ownPersonKeyB64: own,
    ownerKeyB64: facts.ownerKeyB64,
    supersededKeys: superseded,
  });
  if (unrepairableKeyB64 === null) return "nothing";
  const accountIdB64 = superseded.get(unrepairableKeyB64);
  if (accountIdB64 === undefined) return "nothing";
  const state = port.claimState(oldGroupIdB64);

  // An own claim is undecided: step 3.
  if (state.own !== null) {
    const ownClaim = state.own;
    if (ownClaim.messageId === null) {
      // The publish never reported an id (a crash or a failure mid-send).
      // Posting again is harmless: both rows name the same successor.
      await port.postClaim(oldGroupIdB64, ownClaim.successorGroupIdB64);
      return "claimed";
    }
    const cursor = port.cursorOf(oldGroupIdB64);
    if (cursor === undefined || cursor < ownClaim.messageId) return "waiting-for-journal";
    const outcome = claimOutcome(ownClaim.messageId, state.accepted);
    if (outcome.kind === "lost") {
      await port.discardSuccessor(ownClaim.successorGroupIdB64);
      await port.settle(oldGroupIdB64, outcome.winner);
      await port.recordLineage(oldGroupIdB64, outcome.winner.successorGroupIdB64);
      return "lost";
    }
    if (own === null) return "failed";
    const winner: SuccessorClaim = {
      messageId: ownClaim.messageId,
      successorGroupIdB64: ownClaim.successorGroupIdB64,
      senderKeyB64: own,
      senderWasAdmin: facts.adminKeysB64.includes(own),
      adminKeysAtReceiptB64: facts.adminKeysB64,
    };
    await port.settle(oldGroupIdB64, winner);
    return build(port, oldGroupIdB64, winner, facts, own, accountIdB64, unrepairableKeyB64);
  }

  // A claim is held: the conversation's successor is decided.
  if (state.accepted !== null) {
    if (own !== null && state.accepted.senderKeyB64 === own) {
      // This account founded it (this device, or another of the person's):
      // whoever holds the successor keeps building it. The lineage is
      // recorded by `build`, once the successor was delivered.
      return build(port, oldGroupIdB64, state.accepted, facts, own, accountIdB64, unrepairableKeyB64);
    }
    await port.recordLineage(oldGroupIdB64, state.accepted.successorGroupIdB64);
    // R.7 case 3 "the completeness re-check": ANY moved admin or owner, not
    // only the founding account, adds whoever of the old roster the
    // successor still lacks, at the re-admission arm's cadence.
    if (own !== null && facts.kind === "group") {
      return recheckCompleteness(port, state.accepted.successorGroupIdB64, facts, own);
    }
    return "adopted";
  }

  // Steps 1 and 2.
  if (!facts.active || own === null) return "not-eligible";
  // Eligibility first (no network), then the one directory read (R.8).
  if (
    !shouldFoundSuccessor({
      kind: facts.kind,
      unrepairableKeyB64,
      held: state.accepted,
      ownClaimPosted: false,
      membershipEnded: !facts.active,
      ownPersonKeyB64: own,
      ownIsAdmin: facts.adminKeysB64.includes(own),
      adminKeysB64: facts.adminKeysB64,
      supersededKeys: superseded,
      newIdentityHasDevices: true,
    })
  ) {
    return "not-eligible";
  }
  if (!(await port.newIdentityHasDevices(accountIdB64))) return "waiting-for-devices";
  // Nothing is prepared, claimed or moved to while no Welcome could reach
  // the new identity (2026-11-25): the old one-to-one stays the
  // conversation, and a send into it is held.
  if (!(await port.newIdentityReachable(accountIdB64))) return "waiting-for-reach";
  // The claim state is read again after the awaits above: a claim that
  // landed meanwhile means this device stands down (R.6 "to avoid wasted
  // claims a device re-reads the journal head before step 1").
  const again = port.claimState(oldGroupIdB64);
  if (again.accepted !== null || again.own !== null) return "adopted";
  const successorGroupIdB64 = await port.prepareSuccessor(oldGroupIdB64, accountIdB64);
  try {
    await port.postClaim(oldGroupIdB64, successorGroupIdB64);
  } catch (err) {
    // Nothing was announced: the local successor is dropped and the next
    // trigger starts over.
    await port.abandonOwnClaim(oldGroupIdB64);
    await port.discardSuccessor(successorGroupIdB64);
    throw err;
  }
  return "claimed";
}

/**
 * Whether a device that MOVED to the successor runs the completeness
 * re-check there (R.7 case 3: "any moved admin or owner").
 *
 * | holds the successor, membership live | own role in the successor | re-checks |
 * |--------------------------------------|---------------------------|-----------|
 * | no                                   | -                         | no        |
 * | yes                                  | owner or admin            | yes       |
 * | yes                                  | member                    | no        |
 *
 * A plain member's `Add` of another account is refused by the core, so it
 * is not sent.
 */
export function mayRecheckCompleteness(
  successor: ContinuityFacts | undefined,
  ownPersonKeyB64: string,
): boolean {
  if (successor === undefined || !successor.active) return false;
  if (!successor.rosterPersonKeysB64.includes(ownPersonKeyB64)) return false;
  return successor.ownerKeyB64 === ownPersonKeyB64 || successor.adminKeysB64.includes(ownPersonKeyB64);
}

/** The re-check of a device that did not found the successor. */
async function recheckCompleteness(
  port: ContinuityPort,
  successorGroupIdB64: string,
  old: ContinuityFacts,
  own: string,
): Promise<ContinuityStep> {
  const successor = port.facts(successorGroupIdB64);
  if (!mayRecheckCompleteness(successor, own) || successor === undefined) return "adopted";
  const missing = successorMissingPeople({
    oldRosterPersonKeysB64: old.rosterPersonKeysB64,
    successorPersonKeysB64: successor.rosterPersonKeysB64,
    supersededKeysB64: new Set(port.supersededKeys().keys()),
  }).filter((k) => k !== own);
  if (missing.length === 0) return "adopted";
  if (!port.recheckDue(successorGroupIdB64)) return "adopted";
  for (const personKeyB64 of missing) await port.addPerson(successorGroupIdB64, personKeyB64);
  return "rechecked";
}

/** Step 4, for the winner. Every sub-step is idempotent. */
async function build(
  port: ContinuityPort,
  oldGroupIdB64: string,
  winner: SuccessorClaim,
  old: ContinuityFacts,
  own: string,
  accountIdB64: string,
  unrepairableKeyB64: string,
): Promise<ContinuityStep> {
  const successorGroupIdB64 = winner.successorGroupIdB64;
  // Delivered first, then moved to: a successor nobody else is in must never
  // become the conversation (module docs).
  const delivery = await port.deliverSuccessor(oldGroupIdB64, successorGroupIdB64);
  if (delivery === "unreachable") return "waiting-for-reach";
  if (delivery !== "delivered") return "failed";
  await port.recordLineage(oldGroupIdB64, successorGroupIdB64);
  const superseded = new Set(port.supersededKeys().keys());
  if (old.kind === "chat") {
    await port.resendOwnMessages(oldGroupIdB64, successorGroupIdB64, accountIdB64);
    return "built";
  }
  // R.7 case 3: the same people, the same roles and the same owner.
  await port.carryName(oldGroupIdB64, successorGroupIdB64);
  const successor = port.facts(successorGroupIdB64);
  if (successor === undefined) return "failed";
  const missing = successorMissingPeople({
    oldRosterPersonKeysB64: old.rosterPersonKeysB64,
    successorPersonKeysB64: successor.rosterPersonKeysB64,
    supersededKeysB64: superseded,
  }).filter((k) => k !== own);
  for (const personKeyB64 of missing) {
    // A person who cannot be resolved or reached keeps writing in the old
    // group; the completeness re-check (this same pass, later) tries again.
    await port.addPerson(successorGroupIdB64, personKeyB64);
  }
  const after = port.facts(successorGroupIdB64) ?? successor;
  // Roles are restored by the successor's OWNER only (a role change is
  // owner-only), which is this account until the transfer below lands.
  if (after.ownerKeyB64 !== own) return "built";
  const wantedAdmins = successorAdmins({
    oldAdminKeysB64: old.adminKeysB64,
    successorPersonKeysB64: after.rosterPersonKeysB64,
    successorOwnerKeyB64: own,
    supersededKeysB64: superseded,
  });
  const have = new Set(after.adminKeysB64);
  if (wantedAdmins.some((k) => !have.has(k))) {
    await port.setAdmins(successorGroupIdB64, [...new Set([...after.adminKeysB64, ...wantedAdmins])]);
    // One role change per sweep (C-65): the transfer waits for the next pass.
    return "built";
  }
  const newOwnerKeyB64 = port.currentKeyOf(accountIdB64);
  if (
    restoreOwnershipToResetIdentity() &&
    old.ownerKeyB64 === unrepairableKeyB64 &&
    newOwnerKeyB64 !== null &&
    after.rosterPersonKeysB64.includes(newOwnerKeyB64)
  ) {
    await port.transferOwnership(successorGroupIdB64, newOwnerKeyB64, old.adminKeysB64.includes(own));
  }
  return "built";
}
