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
  const inRoster = (keyB64: string): boolean =>
    roster.some((m) => m.signatureKeyB64 === keyB64);

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
