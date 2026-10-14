// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §7 — automatically re-sending a membership request that was
 * refused because a queue was full.
 *
 * ## What this closes
 *
 * `chat/membership-refusal.ts` taught all three clients to *read* the core's
 * {@link https://docs.rs/lakko-mls `RefusalDisposition`}, so a queue-full
 * refusal already renders as `Too many changes are already waiting to apply.
 * Wait for them to land, then try again.` What no client did was **act** on
 * it: the user re-tapped by hand, and the moment to re-tap is one they cannot
 * observe — the queue drains when some other device sweeps, which produces no
 * notification and no visible change in a panel they have probably closed.
 *
 * The core states the property that makes an automatic re-send safe, and it
 * states it narrowly: `RetryLater` means *the identical call succeeds once a
 * committer has swept, with the caller doing nothing else in between*. Two
 * reasons qualify today. This module never names them.
 *
 * ## The trigger is an observation, never a timer — and one rule covers every
 * retryable reason, present and future
 *
 * A schedule is the obvious shape and it is wrong in both directions:
 * ADR-0045 decision 2 gives a request no deadline, so a timer either fires
 * uselessly against a group where nothing changed, or spins forever against a
 * group where no committer is running. The condition is not *time passed*, it
 * is *the queue drained*.
 *
 * So the rule is: **fire when the pending-proposal set is strictly smaller
 * than it was at the refusal.** That is sound for both of today's reasons and
 * — more importantly — it follows from the core's *definition* of
 * `RetryLater` rather than from either variant's name:
 *
 *   - the set only ever shrinks when a commit **merges**, because OpenMLS
 *     empties the whole proposal store on a merge and nothing else removes an
 *     entry (a receipt-gate refusal stores nothing, so it moves no count);
 *   - `RetryLater` is defined as *resolves once a committer has swept*, and a
 *     sweep IS a merged commit;
 *   - therefore a nineteenth `RetryLater` reason is handled correctly by this
 *     rule without anybody editing it, and a reason that would NOT be handled
 *     by it could not have been classified `RetryLater` in the first place.
 *
 * The count compared against is `pendingProposals().length`, which is exactly
 * the number the core's own cap gates on (`pending_proposals().count()`), so
 * this is the literal quantity the refusal was about rather than a proxy for
 * it.
 *
 * **It cannot spin.** No drain, no firing — a quiet group produces no
 * attempts at all, and every attempt is paid for by a real committer sweep.
 * That is the mitigation §7 already prescribes for the superseded case
 * ("re-request only on an observed commit that superseded it, never on a
 * timer"), applied to the sibling case.
 *
 * ## Why the capacity is not consulted, and why it does not need to be
 *
 * `MAX_PENDING_PROPOSALS` is `pub(crate)` and reaches neither FFI surface, so
 * a client cannot test *is there room*. §7.1 records that a client-side
 * literal `64` is the stale-drift class this repo has already been bitten by,
 * and that it fails in the **dangerous** direction — reading as room left when
 * there is none.
 *
 * A shrink is therefore a **necessary** condition rather than a sufficient
 * one: the set is smaller, so a sweep happened, but the client cannot prove
 * the result is under the cap. That is fine, and it is fine for a stated
 * reason rather than by luck — a retry refused `PendingSetFull` again is just
 * another retryable refusal, re-armed against the new count. The mechanism is
 * self-correcting, and each correction still costs one real sweep.
 *
 * ## What is remembered, where, and why losing it is the honest failure
 *
 * IN MEMORY, per `(account, group)`, for the session. Three reasons, and the
 * first is the one that decides it:
 *
 *   1. **The trigger has no meaning across a restart.** The record's whole
 *      content is a comparison baseline — the pending count at the moment of
 *      the refusal. A count captured in one session says nothing about the set
 *      in the next one, so a durable record would either fire on a comparison
 *      that means nothing or need a fresh baseline, which is the same as not
 *      having one.
 *   2. **A deferred action the user never saw start is not the action they
 *      took.** A durable record could re-send a request days later, silently,
 *      in a group the user has stopped thinking about. Losing it is
 *      defensible; that is not.
 *   3. **Losing it degrades to exactly the documented behaviour.** The copy
 *      already tells the user to try again, and the manual path is unchanged.
 *      An automatic retry is strictly a bonus on top of a complete manual
 *      flow, which is why it may fail safe by simply not happening.
 *
 * The consequence is stated rather than hidden: an app closed between the
 * refusal and the drain forgets, and the user must ask again — which is what
 * happens today, on every client, for every refusal.
 *
 * ## Exactly-once
 *
 * The store holds **at most one record per request**, keyed on what the
 * request *is* (see {@link retryKey}), so a second refusal for the same ask
 * replaces rather than accumulates.
 *
 * Firing takes the record out with {@link claimArmedRetry}, a synchronous
 * take-and-remove. Two folds in one turn therefore cannot both fire it: the
 * second gets `undefined`. And the **manual** path claims the same slot before
 * it starts, so a user who taps again while a retry is armed disarms it rather
 * than racing it. That direction matters more than the reverse, because the
 * re-issue itself goes through `withGroupMlsLock`, which is non-reentrant —
 * the driver cannot hold the lock across its own last check, so the check has
 * to be a claim on a slot rather than a read of group state.
 *
 * ## Stopping is a state, never a count
 *
 * There is no attempt cap and there must not be one. A cap measures the wrong
 * thing here: attempts are not a cost the group pays (each one is paid for by
 * a sweep that was going to happen anyway) and a cap would give up on a group
 * that is merely busy — converting a recoverable wait into a permanent failure
 * for no safety gain. §7 already settles the same question for the superseded
 * case: *"There is no attempt cap."*
 *
 * What stops a retry is the world moving so that the request is meaningless:
 * it landed, or its effect is already true, or we are no longer in the group.
 * Each is a fact about group state that the fold reads anyway.
 *
 * ## Approving an external join, and why it took a core change to get here
 *
 * It could not participate at all until `21f6c8c`, and not for a reason any
 * client could work around: `Group::approve_external_join` re-wrapped its
 * inner failure as `Error::Group(format!("external join approve refused:
 * {e}"))`, and `Error::Group` carries no `RefusalReason` — so an approve
 * refused because the pending set was full arrived as an untyped error with no
 * reason at all, indistinguishable from a socket failure. Rule 5 of this
 * module's contract is that an absent reason never retries, so it did not.
 * Inferring the reason from the message text would have been the exact drift
 * class `membership-refusal.ts` exists to end.
 *
 * The core now propagates the typed error and wraps only a reasonless one, so
 * the approve joins the rule on the same terms as every other kind: the port
 * decides whether it arms, and a drained queue decides when it fires.
 *
 * It brings **two** stopping conditions of its own, and the second is the one
 * that is easy to get wrong:
 *
 *   - the joiner is now a member. Someone else's approve was swept, or our own
 *     was — either way the ask is satisfied, and it reads off the roster like
 *     every other `already-applied` test.
 *   - the request has left the **retained quarantine**
 *     (`Group::pending_external_joins`). This is `join-request-gone`, and it
 *     is checked rather than assumed because the quarantine and this store are
 *     two different in-memory lifetimes: the quarantine lives on the live
 *     `Group` handle and is emptied by anything that reloads it from a
 *     snapshot, while this store survives until the tab closes. So the record
 *     CAN outlive the request that justified it, within one session, with no
 *     restart. **Handled, not prevented** — the verdict reads the live set and
 *     drops, rather than firing an approve the core would refuse
 *     `noSuchJoinRequest`.
 *
 * And it must never arm for a request the quarantine did not keep in the first
 * place. That rule already exists as `externalJoinAffordance`, so
 * {@link armedApproveJoinRetry} **asks** it rather than restating `!retained`
 * — one definition of "this request is actionable", which the panel and the
 * retry both read.
 *
 * ## Discrimination MEASURED on the approve arm
 *
 * Each was introduced as a deliberate regression, `npx vitest run` was run, the
 * named test observed to fail, and the probe reverted:
 *
 * 1. drop the affordance guard → `arms exactly when the affordance says the
 *    request is actionable`:
 *
 *        AssertionError: retained=false: expected true to be false
 *
 * 2. remove the retained-quarantine check → two tests, `drops once the request
 *    has left the retained quarantine` and `claims nothing on a torn read of
 *    the retained set`, both `expected { kind: 'fire' }`
 *
 * 3. treat a torn retained read as an empty quarantine → `claims nothing on a
 *    torn read of the retained set`:
 *
 *        AssertionError: expected { kind: 'drop', …(1) } to deeply equal
 *        { kind: 'wait' }
 *
 * 4. stop sharing the `add` arm of `alreadyApplied` → `drops once the joiner is
 *    a member — whoever's approve landed`
 * 5. stop sharing the `add` arm of `alreadyWaiting` → `drops once another
 *    admin's Add for the joiner is already waiting`
 * 6. give the approve the `add:` store slot → `keeps the approve slot apart
 *    from an add of the same person`:
 *
 *        AssertionError: expected 'add:Q0FST0w=' not to be 'add:Q0FST0w='
 *
 * 7. drop `approve-join` from `needsRoleTable` → `drops an approve in a group
 *    that has lost its role table`
 * 8. hard-code the retryable reason instead of asking the port → `follows the
 *    port even when the port is wrong, in both directions`:
 *
 *        AssertionError: a port that says a missing-request refusal is
 *        retryable must be believed: expected null not to be null
 *
 * 9. apply the retained gate to every request kind → five tests, including
 *    `does not read the retained set for any other kind of request`
 */

import {
  externalJoinAffordance,
  refusalReasonOf,
  type RetryPort,
} from "./membership-refusal";
import { recordUnapplied, unappliedFromRequest } from "./unapplied-requests";
import { registerAccountMemory } from "../auth/account-memory";

// ---------------------------------------------------------------------------
// What a retryable request is
// ---------------------------------------------------------------------------

/**
 * A membership request that may be re-issued verbatim.
 *
 * **Every kind is keyed on a 32-byte Ed25519 signature key, never a leaf
 * index.** OpenMLS hands a vacated leaf to the next `Add`, so a stored leaf
 * can silently come to name somebody else — and for a `remove` that is not a
 * stale label, it is removing the wrong person. The leaf the re-issue needs is
 * resolved from the LIVE roster at fire time, which is also what makes
 * "already applied" checkable.
 *
 * `approve-join` carries **one** field and it is doing two jobs at once, which
 * is a property of the core rather than a shortcut here: the joiner's
 * signature key IS the `proposal_ref` handle
 * (`Group::pending_external_joins` returns exactly these keys, and
 * `approve_external_join` looks the request up by matching one). There is no
 * separate proposal reference to carry, and — unlike `add` — no account id
 * either, because the joiner's KeyPackage is already retained in the
 * quarantine. That is what the quarantine is for.
 */
export type RetryableRequest =
  | {
      readonly kind: "add";
      readonly subjectKeyB64: string;
      /** Base64 `account_id`. Carried because re-issuing an add re-fetches the
       *  prospective member's KeyPackage from discovery, which is keyed on the
       *  account rather than on the signature key — and a KeyPackage is
       *  single-use, so the stale one from the refused attempt cannot be
       *  reused. This is the ONE field beyond the identity of the subject, and
       *  it is here because the re-issue genuinely cannot be made without it. */
      readonly subjectAccountIdB64: string;
    }
  | { readonly kind: "remove"; readonly subjectKeyB64: string }
  | { readonly kind: "leave"; readonly subjectKeyB64: string }
  | {
      readonly kind: "set-admin";
      readonly subjectKeyB64: string;
      readonly makeAdmin: boolean;
    }
  | {
      readonly kind: "transfer-ownership";
      readonly subjectKeyB64: string;
      readonly outgoing: "admin" | "member";
    }
  | {
      readonly kind: "approve-join";
      /** The joiner's Ed25519 signature key — both the subject's identity and
       *  the `proposal_ref` handle the approve takes. */
      readonly subjectKeyB64: string;
    };

/**
 * A stable key for one request, so a second refusal for the same ask replaces
 * the first rather than queueing beside it.
 *
 * A role change collapses to one key for the same reason `proposalKey` does:
 * RFC 9420 admits one `GroupContextExtensions` proposal per commit, so two of
 * them could never both be live, and the newer intent is the one the user
 * meant.
 */
export function retryKey(request: RetryableRequest): string {
  switch (request.kind) {
    case "add":
      return `add:${request.subjectKeyB64}`;
    case "remove":
      return `remove:${request.subjectKeyB64}`;
    case "leave":
      return `leave:${request.subjectKeyB64}`;
    case "approve-join":
      // Deliberately NOT the `add:` slot, even though an approve resolves to
      // an `Add` proposal. They are different operations reached by different
      // affordances — one consumes a quarantined request, the other fetches a
      // KeyPackage from discovery — so a manual approve tap must disarm the
      // approve record and leave an armed add alone, and vice versa.
      return `approve-join:${request.subjectKeyB64}`;
    default:
      // Both role-change shapes share ONE slot, because RFC 9420 admits one
      // `GroupContextExtensions` proposal per commit — two of them could never
      // both be live, so a newer role-change intent supersedes an older armed
      // one rather than queueing beside it.
      return "role-change";
  }
}

// ---------------------------------------------------------------------------
// Arming
// ---------------------------------------------------------------------------

/** One request waiting for the queue that refused it to drain. */
export interface ArmedRetry {
  readonly request: RetryableRequest;
  /** The core's reason code, verbatim. For logs only — nothing branches on
   *  it, which is what keeps this module free of the two variant names. */
  readonly reason: string;
  /** `pendingProposals().length` at the moment of the refusal — the baseline
   *  the drain is measured against. */
  readonly pendingCountAtRefusal: number;
}

/**
 * Arm a retry for `err`, or `null` when nothing should be re-sent.
 *
 * `null` covers, deliberately, all four of: not a refusal at all; a
 * `DoNotRetry` refusal; a `NotPermitted` refusal; and a reason this build has
 * never heard of, which is what a core newer than this client produces. The
 * last three are one code path — the port is asked and answers `false` — and
 * that is the point: this module holds no list of retryable reasons and could
 * not disagree with the core if it tried.
 *
 * `retryMaySucceed` is a port rather than a direct import so the tests can
 * prove the module *asks*. Production passes
 * `membershipRefusalRetryMaySucceed` from the wasm surface.
 */
export function armedRetryFor(
  err: unknown,
  request: RetryableRequest,
  pendingCountAtRefusal: number,
  retryMaySucceed: RetryPort,
): ArmedRetry | null {
  const reason = refusalReasonOf(err);
  if (reason === undefined) return null;
  if (!retryMaySucceed(reason)) return null;
  return { request, reason, pendingCountAtRefusal };
}

/**
 * Arm a retry for a refused **external-join approve**, or `null`.
 *
 * Everything {@link armedRetryFor} decides is decided the same way here; the
 * one addition is the `retained` gate, and it is delegated to
 * {@link externalJoinAffordance} rather than written as `if (!retained)`. That
 * function already owns the question *may an approve be offered for this
 * request at all* — the members panel asks it before rendering a button — and
 * an automatic re-send is the same question asked later. Two copies of it
 * could disagree; one cannot.
 *
 * A request the quarantine never kept has nothing for `approveExternalJoin` to
 * look up, so a retry for it could only ever be refused `noSuchJoinRequest`.
 * In production the caller reads `retained` LIVE off
 * `pendingExternalJoins()` at the moment of the refusal, so this is a real
 * read rather than a constant `true`: a quarantine emptied between the tap and
 * the refusal arms nothing.
 */
export function armedApproveJoinRetry(
  err: unknown,
  subjectKeyB64: string,
  pendingCountAtRefusal: number,
  retained: boolean,
  retryMaySucceed: RetryPort,
): ArmedRetry | null {
  if (externalJoinAffordance({ retained }).kind !== "actionable") return null;
  return armedRetryFor(
    err,
    { kind: "approve-join", subjectKeyB64 },
    pendingCountAtRefusal,
    retryMaySucceed,
  );
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/** One roster row as this module reads it. */
export interface RosterMemberView {
  readonly leafIndex: number;
  /** Standard base64 of the member's 32-byte Ed25519 signature key. */
  readonly signatureKeyB64: string;
  /**
   * Standard base64 of the ADR-0050 **account** key the leaf belongs to, when
   * the core resolved one. A person is named by this key (one person, several
   * device leaves), and every request that names a person — an add, a
   * removal, a role change — is keyed on it, so a roster test must match it as
   * well as the leaf key. Absent for a leaf whose binding did not resolve.
   */
  readonly accountKeyB64?: string;
}

/** Is the person or device named by `keyB64` in the roster? Matches the leaf
 *  key AND the account key, because a request may name either (a device row
 *  names a leaf, a person row names an account). */
function inRosterByKey(roster: readonly RosterMemberView[], keyB64: string): boolean {
  return roster.some((m) => m.signatureKeyB64 === keyB64 || m.accountKeyB64 === keyB64);
}

/** Every current leaf of the person or device named by `keyB64`. */
function leavesOfKey(roster: readonly RosterMemberView[], keyB64: string): number[] {
  return roster
    .filter((m) => m.signatureKeyB64 === keyB64 || m.accountKeyB64 === keyB64)
    .map((m) => m.leafIndex);
}

/**
 * Everything the verdict reads, captured in ONE pass.
 *
 * The roster and the pending set must come from the same read: the merge that
 * empties the pending set is the merge that moves the roster, so reading one
 * before a commit and one after would compare two different moments — the same
 * rule `unappliedAddRequests` states for the same pair.
 */
export interface GroupSnapshot {
  /** `null` is a TORN read, not an empty group, and claims nothing. */
  readonly roster: readonly RosterMemberView[] | null;
  /** `proposalKey()` of every live pending record. */
  readonly pendingKeys: readonly string[];
  /** `pendingProposals().length` — the number the core's cap gates on. */
  readonly pendingCount: number;
  readonly roleTable: {
    readonly ownerKeyB64: string;
    readonly adminKeysB64: readonly string[];
  } | null;
  /** ADR-0045 §6.2 — our own leaf has been removed. */
  readonly membershipEnded: boolean;
  /**
   * ADR-0027 — the joiner key of every request the core still holds in its
   * retained external-join quarantine (`pendingExternalJoins()`).
   *
   * `null` is a TORN read, not an empty quarantine, and claims nothing — the
   * same rule `roster` follows, and for the same reason: an empty answer here
   * would look exactly like "the request is gone" and would drop a live retry.
   *
   * Read ONLY for an `approve-join` request. A quarantine says nothing about
   * an add, a removal or a role change, so it must never stop one.
   */
  readonly retainedJoinKeys: readonly string[] | null;
}

/** Why a retry stopped. Every one is a state; none is a count. */
export type RetryDropReason =
  /** We are no longer in the group, so nothing we ask can apply. */
  | "membership-ended"
  /** The effect the request asked for is already true. */
  | "already-applied"
  /** The request is itself in the live pending set — it landed. */
  | "already-waiting"
  /** The group has no role table, so the operation has no committer and the
   *  core would refuse it `NoRoleTable` — a `DoNotRetry` answer. */
  | "no-role-table"
  /** ADR-0027 — the core no longer holds this external-join request, so an
   *  approve has nothing to look up and would be refused
   *  `NoSuchJoinRequest`. */
  | "join-request-gone";

export type RetryVerdict =
  | { readonly kind: "fire" }
  | { readonly kind: "wait" }
  | { readonly kind: "drop"; readonly why: RetryDropReason };

/**
 * What to do with one armed retry, given the group as it is now.
 *
 * Pure and total. The order is load-bearing: every stopping condition is
 * checked BEFORE the drain test, so a request whose effect already happened is
 * dropped rather than re-sent the moment a sweep lands.
 */
export function retryVerdict(armed: ArmedRetry, snapshot: GroupSnapshot): RetryVerdict {
  if (snapshot.membershipEnded) return { kind: "drop", why: "membership-ended" };

  // A torn read claims NOTHING — neither that the request applied nor that it
  // is safe to re-send. Fail-closed in the same direction
  // `unappliedAddRequests` and `membershipRotationDecision` take with the
  // identical signal.
  if (snapshot.roster === null) return { kind: "wait" };

  const request = armed.request;

  if (needsRoleTable(request) && snapshot.roleTable === null) {
    return { kind: "drop", why: "no-role-table" };
  }

  if (alreadyApplied(request, snapshot)) return { kind: "drop", why: "already-applied" };
  if (alreadyWaiting(request, snapshot)) return { kind: "drop", why: "already-waiting" };

  // ADR-0027 — LAST of the stopping conditions, so the two that describe the
  // world (the joiner landed / somebody's Add is waiting) are reported ahead
  // of the one that describes only our own core's bookkeeping. Both of those
  // also empty the quarantine eventually, and "they are already in" is the
  // more useful thing to have said.
  if (request.kind === "approve-join") {
    if (snapshot.retainedJoinKeys === null) return { kind: "wait" };
    if (!snapshot.retainedJoinKeys.includes(request.subjectKeyB64)) {
      return { kind: "drop", why: "join-request-gone" };
    }
  }

  return snapshot.pendingCount < armed.pendingCountAtRefusal
    ? { kind: "fire" }
    : { kind: "wait" };
}

/** Is the effect the request asked for already true of the group? */
function alreadyApplied(request: RetryableRequest, snapshot: GroupSnapshot): boolean {
  const roster = snapshot.roster ?? [];
  const inRoster = (keyB64: string): boolean => inRosterByKey(roster, keyB64);

  switch (request.kind) {
    case "add":
    // An approve asks for exactly one thing — that this person be in the
    // group — so it is satisfied by them being in it, whoever's approve was
    // swept. Sharing the `add` arm is the statement, not a saving.
    case "approve-join":
      return inRoster(request.subjectKeyB64);
    case "remove":
    case "leave":
      return !inRoster(request.subjectKeyB64);
    case "set-admin": {
      const live = snapshot.roleTable;
      if (live === null) return false;
      // The ask is a PREDICATE about one member, not a whole table, so it is
      // satisfied by the live table saying what was asked however it got
      // there — including by somebody else having made the same change.
      return live.adminKeysB64.includes(request.subjectKeyB64) === request.makeAdmin;
    }
    default: {
      const live = snapshot.roleTable;
      if (live === null) return false;
      // The outgoing owner's own new role is deliberately NOT part of the
      // test. The ask that matters — and the only half a peer could observe —
      // is who holds the group. Requiring both would keep re-sending a
      // transfer that has already happened.
      return live.ownerKeyB64 === request.subjectKeyB64;
    }
  }
}

/**
 * Which requests the core refuses outright in a group with no role table.
 *
 * A role change has no committer to sweep it; an approve is refused by
 * `role_table_or_refuse` before it looks at anything else. Both are
 * `NoRoleTable`, a `DoNotRetry` answer, so waiting for a drain would wait
 * forever.
 */
function needsRoleTable(request: RetryableRequest): boolean {
  return (
    request.kind === "set-admin" ||
    request.kind === "transfer-ownership" ||
    request.kind === "approve-join"
  );
}

/**
 * Is this exact request already in the live pending set?
 *
 * The pending set keys a removal on a LEAF (`proposalKey` has nothing else to
 * key it on), while a retry record keys it on a signature key — so the leaf is
 * resolved through the live roster here rather than stored, which is what
 * keeps a vacated-and-reused leaf from making an unrelated pending removal
 * look like ours.
 */
function alreadyWaiting(request: RetryableRequest, snapshot: GroupSnapshot): boolean {
  const pending = new Set(snapshot.pendingKeys);
  switch (request.kind) {
    case "add":
    // ADR-0045 S7 — an approved external join IS an ordinary admin-authored
    // `Add` carrying the joiner's retained KeyPackage, so it lands in the
    // pending set under exactly the key an add of the same person would. That
    // is the point of S7 (indistinguishable from an ordinary add on the wire)
    // and it is why the same lookup answers for both.
    case "approve-join":
      return pending.has(`add:${request.subjectKeyB64}`);
    case "remove":
    case "leave":
      // ADR-0050 — a person may hold several leaves and a removal of the
      // person proposes one Remove per leaf, so any of them pending counts.
      return leavesOfKey(snapshot.roster ?? [], request.subjectKeyB64).some(
        (leaf) => pending.has(`leave:${leaf}`) || pending.has(`remove:${leaf}`),
      );
    default:
      return pending.has("role-change");
  }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/**
 * Armed retries, per `(account, group)` scope key, per request key.
 *
 * MODULE-level for the reason `groupMlsQueue` is: a `ContactsClient` and a
 * `ChatView` are constructed per route, while they all drive the SAME
 * canonical wasm `Identity`. An instance field would let a remount silently
 * split the store in two and lose the exactly-once property with it.
 *
 * In memory on purpose — see the module header. Nothing here touches
 * `localStorage`.
 */
const armedByScope = new Map<string, Map<string, ArmedRetry>>();

/** The scope key. Both halves are required: an account can be a member of a
 *  group on two devices, and a device can hold two accounts. */
export function retryScopeKey(accountIdB64: string, groupIdB64: string): string {
  return `${accountIdB64} ${groupIdB64}`;
}

/** Arm (or re-arm) one request. At most one record per {@link retryKey}. */
export function armRetry(scope: string, armed: ArmedRetry): void {
  let bucket = armedByScope.get(scope);
  if (bucket === undefined) {
    bucket = new Map();
    armedByScope.set(scope, bucket);
  }
  bucket.set(retryKey(armed.request), armed);
}

/**
 * Take the record for `key` out of the store and return it, or `undefined`
 * when there is none.
 *
 * **Synchronous take-and-remove — this is the exactly-once primitive.** Both
 * the retry driver and the manual handler call it, so whichever runs first
 * owns the request and the other finds nothing. It is deliberately not a
 * `get` followed by a `delete` at some later await point: that is the shape
 * that lets two folds in one turn both decide to fire.
 */
export function claimArmedRetry(scope: string, key: string): ArmedRetry | undefined {
  const bucket = armedByScope.get(scope);
  if (bucket === undefined) return undefined;
  const found = bucket.get(key);
  if (found === undefined) return undefined;
  bucket.delete(key);
  if (bucket.size === 0) armedByScope.delete(scope);
  return found;
}

/** Every armed record in `scope`, for the driver to evaluate. Never mutates —
 *  the driver claims what it decides to fire. */
export function armedRetries(scope: string): ArmedRetry[] {
  return [...(armedByScope.get(scope)?.values() ?? [])];
}

/** Forget everything for one group — on leaving it, on sign-out, on
 *  `forgetDevice`. */
export function clearArmedRetries(scope: string): void {
  armedByScope.delete(scope);
}

// Forget device: an armed retry fires a proposal and records it as the
// account's own request, so one armed before the wipe would act for the
// account in its next session on this page. Every scope of the account goes.
registerAccountMemory("membership-retries", (ids) => {
  for (const scope of [...armedByScope.keys()]) {
    if (ids.some((id) => scope.startsWith(`${id} `))) armedByScope.delete(scope);
  }
});

// ===========================================================================
// ADR-0050 section 2.5a point 4 — re-requesting a SUPERSEDED own request
// ===========================================================================
//
// ## What changed, and why the queue-full retry above does not cover it
//
// Since section 2.5a a plain member may sweep **device admissions**, and its
// sweep commits only those plus its own account's self-service proposals.
// OpenMLS empties the whole proposal store on every merged commit, so every
// other pending proposal — an admin's add of a new member, a removal of
// another person, a role change — is **dropped** by that merge. Nothing
// refused it, so no `RefusalReason` exists and the queue-full retry above
// never arms. The UX spec (`proposal-based-membership.md` section 7,
// "Superseded") had designed the answer and nobody built it: the author
// re-requests, silently, with no attempt cap.
//
// ## The record: the INTENT, never the bytes
//
// Every own membership request that went out as a proposal is recorded
// durably, per `(account, group)`, as what was asked rather than what was
// sent: a subject KEY (never a leaf index — OpenMLS reuses a vacated leaf), a
// role change as its DELTA (promote / demote X, transfer to X — never the
// full table, which a stale copy would use to revert somebody else's change),
// an add with the account id it needs to fetch fresh KeyPackages. The record
// is written by `contacts/client.ts` on every proposal it publishes, so the
// user path, the queue-full retry, the device re-admission arm and this
// re-request itself all record through one place.
//
// ## The trigger: an OBSERVED merged commit, never a timer
//
// The record carries the epoch it was last proposed at. A proposal advances
// no epoch, so the group's epoch moving past it means a commit merged — and
// that merge emptied the proposal store. That is the only trigger: a quiet
// group never re-evaluates anything, so the loop is driven by group activity
// and cannot spin (the section 7 mitigation, adopted by ADR-0050 2.5a as the
// answer to that spec's open question 5). There is no attempt cap.
//
// ## Stopping is a state
//
// After a merged commit each record is: LANDED (its effect is true) -> clear;
// its SUBJECT LEFT (a removal / role change about someone no longer here) ->
// clear; STILL PENDING (someone, possibly us, proposed it again at the new
// epoch) -> keep; otherwise SUPERSEDED -> re-request. A re-request that the
// core refuses with a typed reason whose disposition is not `RetryLater`
// (`NotPermitted`, `DoNotRetry`, or a reason this build does not know) STOPS,
// and is listed in `Not applied` so the user sees it. A `RetryLater` refusal
// (a full queue, a role change already pending) keeps the record: its next
// trigger is the next merged commit, which is exactly the event the queue-full
// retry waits for, so it is not armed a second time there. A failure that is
// not a refusal at all (a network error) keeps the record for the same reason.

/** A self-service device request (ADR-0050 sections 2.5 / 2.10). */
export type DeviceRequest =
  | {
      readonly kind: "admit-device";
      /** The device's 32-byte leaf signature key. */
      readonly subjectKeyB64: string;
      /** Base64 `account_id` the device belongs to — what discovery keys its
       *  KeyPackage on, so a re-request can fetch a fresh one. */
      readonly subjectAccountIdB64: string;
    }
  | {
      readonly kind: "retire-device";
      /** The device's 32-byte leaf signature key. */
      readonly subjectKeyB64: string;
    };

/** Every own membership request this device may have to re-derive. */
export type OwnRequest = RetryableRequest | DeviceRequest;

/** The record key — one record per request. A role change collapses to one
 *  key, as in {@link retryKey}: one `GroupContextExtensions` proposal per
 *  commit means the newer intent replaces the older one. */
export function ownRequestKey(request: OwnRequest): string {
  switch (request.kind) {
    case "admit-device":
    case "retire-device":
      return `${request.kind}:${request.subjectKeyB64}`;
    default:
      return retryKey(request);
  }
}

/** One recorded own request. */
export interface OwnRequestRecord {
  readonly request: OwnRequest;
  /** The group epoch the request was last proposed at, as a decimal string
   *  (a `bigint` does not survive JSON). */
  readonly epoch: string;
  /**
   * `add` only — the leaf signature keys of the devices the Add proposals
   * carried (ADR-0050: one proposal per device KeyPackage). The pending set
   * keys an add on the PROPOSED leaf, not on the person, so this is what lets
   * "is it still pending?" be answered for a person add.
   */
  readonly deviceKeysB64?: readonly string[];
}

const OWN_REQUESTS_PREFIX = "lakko.own-requests.v1";

function ownRequestsStoreKey(accountIdB64: string, groupIdB64: string): string {
  return `${OWN_REQUESTS_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

function isString(v: unknown): v is string {
  return typeof v === "string" && v.length > 0;
}

/** Validate one stored request. Anything malformed reads as absent, which
 *  costs a re-request rather than risking one aimed at the wrong subject. */
function parseOwnRequest(v: unknown): OwnRequest | null {
  if (typeof v !== "object" || v === null) return null;
  const r = v as Record<string, unknown>;
  if (!isString(r.subjectKeyB64)) return null;
  const subjectKeyB64 = r.subjectKeyB64;
  switch (r.kind) {
    case "add":
      return isString(r.subjectAccountIdB64)
        ? { kind: "add", subjectKeyB64, subjectAccountIdB64: r.subjectAccountIdB64 }
        : null;
    case "admit-device":
      return isString(r.subjectAccountIdB64)
        ? { kind: "admit-device", subjectKeyB64, subjectAccountIdB64: r.subjectAccountIdB64 }
        : null;
    case "remove":
    case "leave":
    case "approve-join":
    case "retire-device":
      return { kind: r.kind, subjectKeyB64 };
    case "set-admin":
      return typeof r.makeAdmin === "boolean"
        ? { kind: "set-admin", subjectKeyB64, makeAdmin: r.makeAdmin }
        : null;
    case "transfer-ownership":
      return r.outgoing === "admin" || r.outgoing === "member"
        ? { kind: "transfer-ownership", subjectKeyB64, outgoing: r.outgoing }
        : null;
    default:
      return null;
  }
}

/** Read every recorded own request for one group. Never throws. */
export function loadOwnRequests(
  accountIdB64: string,
  groupIdB64: string,
): Map<string, OwnRequestRecord> {
  const out = new Map<string, OwnRequestRecord>();
  try {
    const raw = globalThis.localStorage?.getItem(ownRequestsStoreKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return out;
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    for (const value of Object.values(parsed)) {
      if (typeof value !== "object" || value === null) continue;
      const rec = value as Record<string, unknown>;
      const request = parseOwnRequest(rec.request);
      if (request === null) continue;
      if (typeof rec.epoch !== "string" || !/^[0-9]+$/.test(rec.epoch)) continue;
      const deviceKeys = Array.isArray(rec.deviceKeysB64)
        ? rec.deviceKeysB64.filter(isString)
        : undefined;
      out.set(ownRequestKey(request), {
        request,
        epoch: rec.epoch,
        ...(deviceKeys === undefined ? {} : { deviceKeysB64: deviceKeys }),
      });
    }
  } catch {
    /* a corrupt row reads as empty */
  }
  return out;
}

function saveOwnRequests(
  accountIdB64: string,
  groupIdB64: string,
  records: ReadonlyMap<string, OwnRequestRecord>,
): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const key = ownRequestsStoreKey(accountIdB64, groupIdB64);
    if (records.size === 0) {
      store.removeItem(key);
      return;
    }
    const obj: Record<string, OwnRequestRecord> = {};
    for (const [k, v] of records) obj[k] = v;
    store.setItem(key, JSON.stringify(obj));
  } catch {
    /* best-effort: losing a record costs the automatic re-request only */
  }
}

/**
 * Record (or re-record) one own request, proposed at `epoch`.
 *
 * Called by `contacts/client.ts` after every proposal it publishes. A later
 * record for the same {@link ownRequestKey} replaces the earlier one, which is
 * what re-stamps the epoch after a re-request and what lets a newer role-change
 * intent supersede an older one.
 */
export function recordOwnRequest(
  accountIdB64: string,
  groupIdB64: string,
  request: OwnRequest,
  epoch: bigint,
  deviceKeysB64?: readonly string[],
): void {
  const records = loadOwnRequests(accountIdB64, groupIdB64);
  records.set(ownRequestKey(request), {
    request,
    epoch: epoch.toString(),
    ...(deviceKeysB64 === undefined || deviceKeysB64.length === 0
      ? {}
      : { deviceKeysB64: [...deviceKeysB64] }),
  });
  saveOwnRequests(accountIdB64, groupIdB64, records);
}

/** Forget one record. */
export function forgetOwnRequest(accountIdB64: string, groupIdB64: string, key: string): void {
  const records = loadOwnRequests(accountIdB64, groupIdB64);
  if (records.delete(key)) saveOwnRequests(accountIdB64, groupIdB64, records);
}

/** Move one record's epoch forward without touching anything else — the
 *  synchronous claim a re-request takes before it awaits anything. */
function stampOwnRequest(
  accountIdB64: string,
  groupIdB64: string,
  key: string,
  epoch: bigint,
): void {
  const records = loadOwnRequests(accountIdB64, groupIdB64);
  const existing = records.get(key);
  if (existing === undefined) return;
  records.set(key, { ...existing, epoch: epoch.toString() });
  saveOwnRequests(accountIdB64, groupIdB64, records);
}

/** Drop every record for one account — `recover` / `forgetDevice` hygiene. */
export function clearOwnRequestsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${OWN_REQUESTS_PREFIX}.${accountIdB64}.`)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

/** Drop every record for one group — `Delete this chat` hygiene. */
export function clearOwnRequestsForGroup(accountIdB64: string, groupIdB64: string): void {
  try {
    globalThis.localStorage?.removeItem(ownRequestsStoreKey(accountIdB64, groupIdB64));
  } catch {
    /* best-effort */
  }
}

// ---------------------------------------------------------------------------
// The superseded verdict
// ---------------------------------------------------------------------------

/** The group as the superseded verdict reads it: {@link GroupSnapshot} plus
 *  the epoch, captured in the same pass. */
export interface IntentSnapshot extends GroupSnapshot {
  readonly epoch: bigint;
}

/** Why a record stopped without its effect landing — each one is listed in
 *  `Not applied`. */
export type IntentStopReason =
  | "membership-ended"
  | "no-role-table"
  | "join-request-gone"
  | "refused";

export type SupersededVerdict =
  /** No merged commit since the record was written (or a torn read): nothing
   *  to decide. */
  | { readonly kind: "no-commit" }
  /** The effect is true. Cleared silently. */
  | { readonly kind: "landed" }
  /** A removal / role change about someone no longer in the group. Cleared
   *  silently. */
  | { readonly kind: "subject-left" }
  /** Proposed again at the current epoch (by us or anyone). Kept, re-stamped. */
  | { readonly kind: "still-pending" }
  /** A commit merged without carrying it. Re-request from the intent. */
  | { readonly kind: "rerequest" }
  /** It cannot be re-requested. Cleared and listed in `Not applied`. */
  | { readonly kind: "stop"; readonly why: IntentStopReason };

function requestNeedsRoleTable(request: OwnRequest): boolean {
  return (
    request.kind === "set-admin" ||
    request.kind === "transfer-ownership" ||
    request.kind === "approve-join"
  );
}

/** Is the effect the request asked for true of the group? */
function intentLanded(record: OwnRequestRecord, snapshot: IntentSnapshot): boolean {
  const roster = snapshot.roster ?? [];
  const request = record.request;
  switch (request.kind) {
    case "add":
      return (
        inRosterByKey(roster, request.subjectKeyB64) ||
        (record.deviceKeysB64 ?? []).some((k) => roster.some((m) => m.signatureKeyB64 === k))
      );
    case "approve-join":
    case "admit-device":
      return inRosterByKey(roster, request.subjectKeyB64);
    case "remove":
    case "leave":
    case "retire-device":
      return !inRosterByKey(roster, request.subjectKeyB64);
    default:
      // A role change: the predicate the user asked for, read off the live
      // table — shared with the queue-full retry so the two cannot disagree.
      return alreadyApplied(request, snapshot);
  }
}

/** A role change about somebody who is no longer in the group. */
function intentSubjectLeft(request: OwnRequest, snapshot: IntentSnapshot): boolean {
  if (request.kind !== "set-admin" && request.kind !== "transfer-ownership") return false;
  return !inRosterByKey(snapshot.roster ?? [], request.subjectKeyB64);
}

/** Is this request (by anyone) in the live pending set? */
function intentPending(record: OwnRequestRecord, snapshot: IntentSnapshot): boolean {
  const pending = new Set(snapshot.pendingKeys);
  const request = record.request;
  switch (request.kind) {
    case "add":
      return [request.subjectKeyB64, ...(record.deviceKeysB64 ?? [])].some((k) =>
        pending.has(`add:${k}`),
      );
    case "admit-device":
      return pending.has(`add:${request.subjectKeyB64}`);
    case "retire-device":
      return leavesOfKey(snapshot.roster ?? [], request.subjectKeyB64).some(
        (leaf) => pending.has(`remove:${leaf}`) || pending.has(`leave:${leaf}`),
      );
    default:
      return alreadyWaiting(request, snapshot);
  }
}

/**
 * What to do with one recorded own request, given the group as it is now.
 *
 * Pure and total. The epoch gate comes first after the two state checks that
 * need no commit to be meaningful: without a merged commit since the record
 * was written there is nothing to decide, which is what keeps this from ever
 * firing on anything but an observed commit.
 */
export function supersededVerdict(
  record: OwnRequestRecord,
  snapshot: IntentSnapshot,
): SupersededVerdict {
  const request = record.request;
  if (snapshot.membershipEnded) {
    // Our own leave landing IS our membership ending.
    return request.kind === "leave"
      ? { kind: "landed" }
      : { kind: "stop", why: "membership-ended" };
  }
  // A torn read claims nothing — the same rule the queue-full verdict follows.
  if (snapshot.roster === null) return { kind: "no-commit" };
  let recordedAt: bigint;
  try {
    recordedAt = BigInt(record.epoch);
  } catch {
    return { kind: "no-commit" };
  }
  if (snapshot.epoch <= recordedAt) return { kind: "no-commit" };

  if (intentLanded(record, snapshot)) return { kind: "landed" };
  if (intentSubjectLeft(request, snapshot)) return { kind: "subject-left" };
  if (intentPending(record, snapshot)) return { kind: "still-pending" };
  if (requestNeedsRoleTable(request) && snapshot.roleTable === null) {
    return { kind: "stop", why: "no-role-table" };
  }
  if (request.kind === "approve-join") {
    // The core consumes the retained KeyPackage when it approves, so a
    // re-approve is possible only while the joiner's request is retained
    // again (it asked again). Without it there is nothing to approve from.
    if (snapshot.retainedJoinKeys === null) return { kind: "no-commit" };
    if (!snapshot.retainedJoinKeys.includes(request.subjectKeyB64)) {
      return { kind: "stop", why: "join-request-gone" };
    }
  }
  return { kind: "rerequest" };
}

/**
 * Does a failed re-request stop, or wait for the next merged commit?
 *
 * Asks the core, never a list here: a typed refusal the core classifies
 * `RetryLater` keeps the record (its next trigger is the next merged commit,
 * the same event the queue-full retry waits for), any other typed refusal —
 * `NotPermitted`, `DoNotRetry`, or a reason this build does not know — stops
 * it. An error that is not a refusal at all (a network failure, a missing
 * KeyPackage) keeps it: nothing says the next attempt will fail, and the next
 * attempt is paid for by a real commit.
 */
export function rerequestFailureStops(err: unknown, retryMaySucceed: RetryPort): boolean {
  const reason = refusalReasonOf(err);
  if (reason === undefined) return false;
  return !retryMaySucceed(reason);
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/** Everything the driver needs from its host (the chat view or the background
 *  receive loop). */
export interface RerequestPorts {
  readonly accountIdB64: string;
  readonly groupIdB64: string;
  /** The group now, in one pass. */
  snapshot(): IntentSnapshot;
  /**
   * Re-issue one request through the SAME propose path the user path uses
   * (same locks, same ADR-0040 / ADR-0057 steps). The proposal path records
   * the request again itself, at the epoch it was proposed at.
   */
  rerequest(request: OwnRequest): Promise<void>;
  /** The core's `RetryLater` test (`membershipRefusalRetryMaySucceed`). */
  readonly retryMaySucceed: RetryPort;
}

/** What one pass did, for logs and tests. */
export interface RerequestPassSummary {
  readonly rerequested: number;
  readonly cleared: number;
  readonly stopped: number;
}

/** One pass at a time per `(account, group)` in this realm. */
const rerequestInFlight = new Set<string>();

/**
 * Evaluate every recorded own request of one group and re-request the
 * superseded ones.
 *
 * Cheap and side-effect-free when nothing was recorded or no commit has merged
 * since: it reads the store, compares epochs and returns. Exactly-once within
 * this realm: a record is re-stamped to the current epoch **synchronously**
 * before any await, so a second pass started meanwhile sees no new commit for
 * it. Silent: nothing it does reaches the UI except a stopped request, which
 * goes to `Not applied`.
 */
export async function runSupersededRerequests(
  ports: RerequestPorts,
): Promise<RerequestPassSummary> {
  const scope = retryScopeKey(ports.accountIdB64, ports.groupIdB64);
  const empty = { rerequested: 0, cleared: 0, stopped: 0 };
  if (rerequestInFlight.has(scope)) return empty;
  const records = loadOwnRequests(ports.accountIdB64, ports.groupIdB64);
  if (records.size === 0) return empty;
  const snapshot = ports.snapshot();
  let cleared = 0;
  let stopped = 0;
  const due: OwnRequestRecord[] = [];
  for (const [key, record] of records) {
    const verdict = supersededVerdict(record, snapshot);
    switch (verdict.kind) {
      case "no-commit":
        break;
      case "landed":
      case "subject-left":
        forgetOwnRequest(ports.accountIdB64, ports.groupIdB64, key);
        cleared += 1;
        break;
      case "still-pending":
        stampOwnRequest(ports.accountIdB64, ports.groupIdB64, key, snapshot.epoch);
        break;
      case "stop":
        forgetOwnRequest(ports.accountIdB64, ports.groupIdB64, key);
        recordUnapplied(ports.accountIdB64, ports.groupIdB64, [
          unappliedFromRequest(record.request),
        ]);
        stopped += 1;
        break;
      case "rerequest":
        // The claim: from here on this record waits for the NEXT commit.
        stampOwnRequest(ports.accountIdB64, ports.groupIdB64, key, snapshot.epoch);
        due.push(record);
        break;
    }
  }
  if (due.length === 0) return { rerequested: 0, cleared, stopped };
  rerequestInFlight.add(scope);
  let rerequested = 0;
  try {
    for (const record of due) {
      try {
        await ports.rerequest(record.request);
        rerequested += 1;
      } catch (err) {
        if (rerequestFailureStops(err, ports.retryMaySucceed)) {
          forgetOwnRequest(ports.accountIdB64, ports.groupIdB64, ownRequestKey(record.request));
          recordUnapplied(ports.accountIdB64, ports.groupIdB64, [
            unappliedFromRequest(record.request),
          ]);
          stopped += 1;
        }
        // Otherwise the record stays, stamped at this epoch, and the next
        // merged commit re-evaluates it.
      }
    }
  } finally {
    rerequestInFlight.delete(scope);
  }
  return { rerequested, cleared, stopped };
}
