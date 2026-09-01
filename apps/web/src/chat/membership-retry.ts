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
 * ## What is deliberately NOT retried, and it is a core finding rather than a
 * scope choice
 *
 * An **approved external join** cannot participate, because the core destroys
 * the reason on that path: `Group::approve_external_join` re-wraps its inner
 * failure as `Error::Group(format!("external join approve refused: {e}"))`,
 * and `Error::Group` carries no `RefusalReason`. So an approve refused because
 * the pending set was full arrives at the client as an untyped error with no
 * reason at all — indistinguishable from any other failure. Rule 5 of this
 * module's contract is that an absent reason never retries, so it does not.
 * Fixing it is a one-line core change (propagate rather than re-wrap) and is
 * reported rather than worked around here: inferring the reason from the
 * message text is precisely the drift class `membership-refusal.ts` exists to
 * end.
 */

import { refusalReasonOf, type RetryPort } from "./membership-refusal";

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
 * `approve-join` is absent by a finding rather than an omission — see the
 * module header.
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

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/** One roster row as this module reads it. */
export interface RosterMemberView {
  readonly leafIndex: number;
  /** Standard base64 of the member's 32-byte Ed25519 signature key. */
  readonly signatureKeyB64: string;
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
}

/** Why a retry stopped. Every one is a state; none is a count. */
export type RetryDropReason =
  /** We are no longer in the group, so nothing we ask can apply. */
  | "membership-ended"
  /** The effect the request asked for is already true. */
  | "already-applied"
  /** The request is itself in the live pending set — it landed. */
  | "already-waiting"
  /** The group has no role table, so a role change has no committer and the
   *  core would refuse it `NoRoleTable` — a `DoNotRetry` answer. */
  | "no-role-table";

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

  if (isRoleChange(request) && snapshot.roleTable === null) {
    return { kind: "drop", why: "no-role-table" };
  }

  if (alreadyApplied(request, snapshot)) return { kind: "drop", why: "already-applied" };
  if (alreadyWaiting(request, snapshot)) return { kind: "drop", why: "already-waiting" };

  return snapshot.pendingCount < armed.pendingCountAtRefusal
    ? { kind: "fire" }
    : { kind: "wait" };
}

/** Is the effect the request asked for already true of the group? */
function alreadyApplied(request: RetryableRequest, snapshot: GroupSnapshot): boolean {
  const roster = snapshot.roster ?? [];
  const inRoster = (keyB64: string): boolean =>
    roster.some((m) => m.signatureKeyB64 === keyB64);

  switch (request.kind) {
    case "add":
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

/** Both role-change shapes, which share the single RFC 9420 GCE slot. */
function isRoleChange(request: RetryableRequest): boolean {
  return request.kind === "set-admin" || request.kind === "transfer-ownership";
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
      return pending.has(`add:${request.subjectKeyB64}`);
    case "remove":
    case "leave": {
      const leaf = (snapshot.roster ?? []).find(
        (m) => m.signatureKeyB64 === request.subjectKeyB64,
      )?.leafIndex;
      if (leaf === undefined) return false;
      return pending.has(`${request.kind}:${leaf}`) || pending.has(`remove:${leaf}`);
    }
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
