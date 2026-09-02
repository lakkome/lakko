// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// MARK: - ADR-0045 UX §7 — automatically re-sending a queue-full refusal
//
// iOS mirror of the web reference `apps/web/src/chat/membership-retry.ts`.
//
// ## What this closes
//
// `MembershipRefusal.swift` taught this client to *read* the core's
// `RefusalDisposition`, so a queue-full refusal already renders as `Too many
// changes are already waiting to apply. Wait for them to land, then try
// again.` What it did not do was **act** on it: the user re-tapped by hand,
// and the moment to re-tap is one they cannot observe — the queue drains when
// some other device sweeps, which produces no notification and no visible
// change in a panel they have probably closed.
//
// The core states the property that makes an automatic re-send safe, and it
// states it narrowly: `RetryLater` means *the identical call succeeds once a
// committer has swept, with the caller doing nothing else in between*. Two
// reasons qualify today. This file never names them.
//
// ## The trigger is an observation, never a timer — and ONE rule covers every
// retryable reason, present and future
//
// A schedule is the obvious shape and it is wrong in both directions: ADR-0045
// decision 2 gives a request no deadline, so a timer either fires uselessly
// against a group where nothing changed, or spins forever against a group
// where no committer is running. The condition is not *time passed*, it is
// *the queue drained*.
//
// So the rule is: **fire when the pending-proposal set is strictly smaller
// than it was at the refusal.** That is sound for both of today's reasons and
// — more importantly — it follows from the core's *definition* of
// `RetryLater` rather than from either variant's name:
//
//   - the set only ever shrinks when a commit **merges**, because OpenMLS
//     empties the whole proposal store on a merge and nothing else removes an
//     entry (a receipt-gate refusal stores nothing, so it moves no count);
//   - `RetryLater` is defined as *resolves once a committer has swept*, and a
//     sweep IS a merged commit;
//   - therefore a nineteenth `RetryLater` reason is handled correctly by this
//     rule without anybody editing it, and a reason that would NOT be handled
//     by it could not have been classified `RetryLater` in the first place.
//
// The count compared against is `pendingProposals().count`, which is exactly
// the number the core's own cap gates on, so this is the literal quantity the
// refusal was about rather than a proxy for it.
//
// **It cannot spin.** No drain, no firing — a quiet group produces no attempts
// at all, and every attempt is paid for by a real committer sweep. That is the
// mitigation §7 already prescribes for the superseded case ("re-request only
// on an observed commit that superseded it, never on a timer"), applied to the
// sibling case.
//
// ## Why the capacity is not consulted, and why it does not need to be
//
// `MAX_PENDING_PROPOSALS` is `pub(crate)` and reaches neither FFI surface, so
// a client cannot test *is there room*. §7.1 records that a client-side
// literal `64` is the stale-drift class this repo has already been bitten by,
// and that it fails in the **dangerous** direction — reading as room left when
// there is none.
//
// A shrink is therefore a **necessary** condition rather than a sufficient
// one, and that is fine for a stated reason rather than by luck: a retry
// refused `PendingSetFull` again is just another retryable refusal, re-armed
// against the new count. The mechanism is self-correcting, and each correction
// still costs one real sweep.
//
// ## What is remembered, where, and why losing it is the honest failure
//
// IN MEMORY, per `(account, group)`, for the session. Three reasons, and the
// first decides it:
//
//   1. **The trigger has no meaning across a restart.** The record's whole
//      content is a comparison baseline — the pending count at the refusal. A
//      count captured in one session says nothing about the set in the next.
//   2. **A deferred action the user never saw start is not the action they
//      took.** A durable record could re-send days later, silently, in a group
//      they have stopped thinking about.
//   3. **Losing it degrades to exactly the documented behaviour.** The copy
//      already tells the user to try again and the manual path is unchanged,
//      so an automatic retry is strictly a bonus on top of a complete manual
//      flow — which is why it may fail safe by simply not happening.
//
// ## Exactly-once
//
// The store holds **at most one record per request**, keyed on what the
// request *is* (``RetryableRequest/key``). Firing takes the record out with
// ``MembershipRetryStore/claim(scope:key:)``, a lock-guarded take-and-remove,
// so two evaluations cannot both fire it. The **manual** path claims the same
// slot before it starts, so a user tapping again disarms rather than races —
// which matters more than the reverse, because the re-issue goes through the
// repository's own commit lock and the driver cannot hold that across its own
// last check.
//
// The lock is explicit rather than relying on `@MainActor`. That safety would
// be incidental — one added `await` inside the claim would reopen the window
// silently — and this repo has already had that exact lesson (ADR-0044's
// serializing-lock arc, where iOS's `@MainActor` mutual exclusion was found to
// be real but accidental).
//
// ## Stopping is a state, never a count
//
// There is no attempt cap and there must not be one. A cap measures the wrong
// thing: attempts are not a cost the group pays (each is paid for by a sweep
// that was going to happen anyway) and a cap would give up on a group that is
// merely busy, converting a recoverable wait into a permanent failure. §7
// already settles the same question for the superseded case: *"There is no
// attempt cap."*
//
// ## Approving an external join, and why it took a core change to get here
//
// It could not participate at all until `21f6c8c`, and not for a reason any
// client could work around: `Group::approve_external_join` re-wrapped its
// inner failure as `Error::Group(format!("external join approve refused:
// {e}"))`, and `Error::Group` carries no `RefusalReason` — so an approve
// refused because the pending set was full arrived with no reason at all,
// indistinguishable from a socket failure. An absent reason never retries, so
// it did not. Inferring the reason from message text would have been the exact
// drift class `MembershipRefusal.swift` exists to end.
//
// The core now propagates the typed error and wraps only a reasonless one, so
// the approve joins the rule on the same terms as every other kind: the port
// decides whether it arms, and a drained queue decides when it fires.
//
// It brings **two** stopping conditions of its own, and the second is the one
// that is easy to get wrong:
//
//   - the joiner is now a member. Someone else's approve was swept, or our own
//     was — either way the ask is satisfied, and it reads off the roster like
//     every other `alreadyApplied` test.
//   - the request has left the **retained quarantine**
//     (`Group.pendingExternalJoins()`). This is `joinRequestGone`, and it is
//     checked rather than assumed because the quarantine and this store are two
//     different in-memory lifetimes: the quarantine lives on the live `Group`
//     handle and is emptied by anything that reloads it from a snapshot, while
//     this store lives as long as the view model. So the record CAN outlive the
//     request that justified it, within one session, with no restart.
//     **Handled, not prevented** — the verdict reads the live set and drops,
//     rather than firing an approve the core would refuse `noSuchJoinRequest`.
//
// And it must never arm for a request the quarantine did not keep in the first
// place. That rule already exists as ``externalJoinAffordance(retained:)``, so
// ``armedApproveJoinRetry(for:subjectKeyB64:pendingCountAtRefusal:retained:reason:retryMaySucceed:)``
// **asks** it rather than restating `!retained` — one definition of "this
// request is actionable", which the panel and the retry both read.
//
// ## Discrimination MEASURED on the approve arm
//
// Each was introduced as a deliberate regression, the suite was run, the named
// test observed to fail, and the probe reverted:
//
//   1. drop the affordance guard → `testArmsExactlyWhenTheAffordanceSaysTheRequestIsActionable`
//   2. remove the retained-quarantine check → `testDropsAnApproveOnceTheRequestHasLeftTheRetainedQuarantine`
//      + `testClaimsNothingOnATornReadOfTheRetainedSet`
//   3. treat a torn retained read as an empty quarantine → `testClaimsNothingOnATornReadOfTheRetainedSet`
//   4. stop sharing the `add` arm of `alreadyApplied` → `testDropsAnApproveOnceTheJoinerIsAMember`
//   5. stop sharing the `add` arm of `alreadyWaiting` → `testDropsAnApproveOnceAnAddForTheJoinerIsAlreadyWaiting`
//   6. give the approve the `add:` store slot → `testKeepsTheApproveSlotApartFromAnAddOfTheSamePerson`
//   7. drop `approveJoin` from `needsRoleTable` → `testDropsAnApproveInAGroupThatHasLostItsRoleTable`
//   8. hard-code the retryable reason instead of asking the port →
//      `testTheApproveArmFollowsThePortEvenWhenThePortIsWrong`
//   9. apply the retained gate to every kind → `testDoesNotReadTheRetainedSetForAnyOtherKindOfRequest`

// MARK: - What a retryable request is

/// A membership request that may be re-issued verbatim.
///
/// **Every case is keyed on a 32-byte Ed25519 signature key, never a leaf
/// index.** OpenMLS hands a vacated leaf to the next `Add`, so a stored leaf
/// can silently come to name somebody else — and for a `remove` that is not a
/// stale label, it is removing the wrong person. The leaf the re-issue needs is
/// resolved from the LIVE roster at fire time, which is also what makes
/// "already applied" checkable.
public enum RetryableRequest: Equatable {
    /// `subjectAccountIdB64` is carried because re-issuing an add re-fetches
    /// the prospective member's KeyPackage from discovery, which is keyed on
    /// the account rather than the signature key — and a KeyPackage is
    /// single-use, so the stale one from the refused attempt cannot be reused.
    /// It is the one field beyond the subject's identity, and it is here
    /// because the re-issue genuinely cannot be made without it.
    case add(subjectKeyB64: String, subjectAccountIdB64: String)
    case remove(subjectKeyB64: String)
    case leave(subjectKeyB64: String)
    case setAdmin(subjectKeyB64: String, makeAdmin: Bool)
    case transferOwnership(subjectKeyB64: String, outgoing: OutgoingOwner)
    /// Approving a retained external-join request (ADR-0027).
    ///
    /// **One field, doing two jobs at once** — a property of the core rather
    /// than a shortcut here: the joiner's signature key IS the `proposal_ref`
    /// handle (`Group.pendingExternalJoins()` returns exactly these keys, and
    /// `approveExternalJoin` looks the request up by matching one). There is no
    /// separate proposal reference to carry, and — unlike `add` — no account id
    /// either, because the joiner's KeyPackage is already retained in the
    /// quarantine. That is what the quarantine is for.
    case approveJoin(subjectKeyB64: String)

    /// A stable key, so a second refusal for the same ask replaces the first
    /// rather than queueing beside it.
    ///
    /// Both role-change shapes share ONE slot, because RFC 9420 admits one
    /// `GroupContextExtensions` proposal per commit — two could never both be
    /// live, so a newer role-change intent supersedes an older armed one.
    public var key: String {
        switch self {
        case .add(let k, _): return "add:\(k)"
        case .remove(let k): return "remove:\(k)"
        case .leave(let k): return "leave:\(k)"
        // Deliberately NOT the `add:` slot, even though an approve resolves to
        // an `Add` proposal. They are different operations reached by different
        // affordances — one consumes a quarantined request, the other fetches a
        // KeyPackage from discovery — so a manual approve tap must disarm the
        // approve record and leave an armed add alone, and vice versa.
        case .approveJoin(let k): return "approve-join:\(k)"
        case .setAdmin, .transferOwnership: return "role-change"
        }
    }

    /// Which requests the core refuses outright in a group with no role table.
    ///
    /// A role change has no committer to sweep it; an approve is refused by
    /// `role_table_or_refuse` before it looks at anything else. Both are
    /// `noRoleTable`, a `DoNotRetry` answer, so waiting for a drain would wait
    /// forever.
    var needsRoleTable: Bool {
        switch self {
        case .setAdmin, .transferOwnership, .approveJoin: return true
        default: return false
        }
    }
}

// MARK: - Arming

/// One request waiting for the queue that refused it to drain.
public struct ArmedRetry: Equatable {
    public let request: RetryableRequest
    /// The core's reason. For logs only — nothing branches on it, which is what
    /// keeps this file free of the two retryable variant names.
    public let reason: FfiRefusalReason
    /// `pendingProposals().count` at the refusal — the drain baseline.
    public let pendingCountAtRefusal: Int

    public init(
        request: RetryableRequest,
        reason: FfiRefusalReason,
        pendingCountAtRefusal: Int
    ) {
        self.request = request
        self.reason = reason
        self.pendingCountAtRefusal = pendingCountAtRefusal
    }
}

/// Arm a retry for `error`, or `nil` when nothing should be re-sent.
///
/// `nil` covers, deliberately, all four of: not a refusal at all; a
/// `DoNotRetry` refusal; a `NotPermitted` one; and a reason this build has
/// never heard of. The last three are one code path — the port is asked and
/// answers `false` — and that is the point: this file holds no list of
/// retryable reasons and could not disagree with the core if it tried.
///
/// `retryMaySucceed` is a parameter rather than a direct call so the tests can
/// prove the module *asks*. Production passes
/// ``membershipRefusalRetryMaySucceed``.
public func armedRetry(
    for error: Error,
    request: RetryableRequest,
    pendingCountAtRefusal: Int,
    reason explicitReason: FfiRefusalReason? = nil,
    retryMaySucceed: (FfiRefusalReason) -> Bool
) -> ArmedRetry? {
    guard let reason = explicitReason ?? membershipRefusalReason(of: error) else { return nil }
    guard retryMaySucceed(reason) else { return nil }
    return ArmedRetry(
        request: request,
        reason: reason,
        pendingCountAtRefusal: pendingCountAtRefusal
    )
}

/// Arm a retry for a refused **external-join approve**, or `nil`.
///
/// Everything ``armedRetry(for:request:pendingCountAtRefusal:reason:retryMaySucceed:)``
/// decides is decided the same way here; the one addition is the `retained`
/// gate, and it is delegated to ``externalJoinAffordance(retained:)`` rather
/// than written as `if !retained`. That function already owns the question *may
/// an approve be offered for this request at all* — the members panel asks it
/// before rendering a button — and an automatic re-send is the same question
/// asked later. Two copies of it could disagree; one cannot.
///
/// A request the quarantine never kept has nothing for `approveExternalJoin` to
/// look up, so a retry for it could only ever be refused `noSuchJoinRequest`.
/// In production the caller reads `retained` LIVE off `pendingExternalJoins()`
/// at the moment of the refusal, so this is a real read rather than a constant
/// `true`: a quarantine emptied between the tap and the refusal arms nothing.
public func armedApproveJoinRetry(
    for error: Error,
    subjectKeyB64: String,
    pendingCountAtRefusal: Int,
    retained: Bool,
    reason explicitReason: FfiRefusalReason? = nil,
    retryMaySucceed: (FfiRefusalReason) -> Bool
) -> ArmedRetry? {
    guard externalJoinAffordance(retained: retained) == .actionable else { return nil }
    return armedRetry(
        for: error,
        request: .approveJoin(subjectKeyB64: subjectKeyB64),
        pendingCountAtRefusal: pendingCountAtRefusal,
        reason: explicitReason,
        retryMaySucceed: retryMaySucceed
    )
}

// MARK: - The verdict

/// One roster row as the verdict reads it.
public struct RetryRosterMember: Equatable {
    public let leafIndex: UInt32
    /// Standard base64 of the member's 32-byte Ed25519 signature key.
    public let signatureKeyB64: String

    public init(leafIndex: UInt32, signatureKeyB64: String) {
        self.leafIndex = leafIndex
        self.signatureKeyB64 = signatureKeyB64
    }
}

/// The ADR-0022 role table as the verdict reads it.
public struct RetryRoleTable: Equatable {
    public let ownerKeyB64: String
    public let adminKeysB64: [String]

    public init(ownerKeyB64: String, adminKeysB64: [String]) {
        self.ownerKeyB64 = ownerKeyB64
        self.adminKeysB64 = adminKeysB64
    }
}

/// Everything the verdict reads, captured in ONE pass.
///
/// The roster and the pending set must come from the same read: the merge that
/// empties the pending set is the merge that moves the roster, so reading one
/// before a commit and one after would compare two different moments.
public struct RetryGroupSnapshot: Equatable {
    /// `nil` is a TORN read, not an empty group, and claims nothing.
    public let roster: [RetryRosterMember]?
    /// ``proposalKey(_:)`` of every live pending record.
    public let pendingKeys: [String]
    /// `pendingProposals().count` — the number the core's cap gates on.
    public let pendingCount: Int
    public let roleTable: RetryRoleTable?
    /// ADR-0045 §6.2 — our own leaf has been removed.
    public let membershipEnded: Bool
    /// ADR-0027 — the joiner key of every request the core still holds in its
    /// retained external-join quarantine (`pendingExternalJoins()`).
    ///
    /// `nil` is a TORN read, not an empty quarantine, and claims nothing — the
    /// same rule ``roster`` follows, and for the same reason: an empty answer
    /// here would look exactly like "the request is gone" and would drop a live
    /// retry.
    ///
    /// Read ONLY for an ``RetryableRequest/approveJoin(subjectKeyB64:)``
    /// request. A quarantine says nothing about an add, a removal or a role
    /// change, so it must never stop one.
    public let retainedJoinKeys: [String]?

    public init(
        roster: [RetryRosterMember]?,
        pendingKeys: [String],
        pendingCount: Int,
        roleTable: RetryRoleTable?,
        membershipEnded: Bool,
        retainedJoinKeys: [String]?
    ) {
        self.roster = roster
        self.pendingKeys = pendingKeys
        self.pendingCount = pendingCount
        self.roleTable = roleTable
        self.membershipEnded = membershipEnded
        self.retainedJoinKeys = retainedJoinKeys
    }
}

/// Why a retry stopped. Every one is a state; none is a count.
public enum RetryDropReason: Equatable {
    /// We are no longer in the group, so nothing we ask can apply.
    case membershipEnded
    /// The effect the request asked for is already true.
    case alreadyApplied
    /// The request is itself in the live pending set — it landed.
    case alreadyWaiting
    /// The group has no role table, so the operation has no committer and the
    /// core would refuse it `noRoleTable` — a `DoNotRetry` answer.
    case noRoleTable
    /// ADR-0027 — the core no longer holds this external-join request, so an
    /// approve has nothing to look up and would be refused `noSuchJoinRequest`.
    case joinRequestGone
}

public enum RetryVerdict: Equatable {
    case fire
    case wait
    case drop(RetryDropReason)
}

/// What to do with one armed retry, given the group as it is now.
///
/// Pure and total. The order is load-bearing: every stopping condition is
/// checked BEFORE the drain test, so a request whose effect already happened is
/// dropped rather than re-sent the moment a sweep lands.
public func retryVerdict(_ armed: ArmedRetry, _ snapshot: RetryGroupSnapshot) -> RetryVerdict {
    if snapshot.membershipEnded { return .drop(.membershipEnded) }

    // A torn read claims NOTHING — neither that the request applied nor that
    // it is safe to re-send. Fail-closed in the same direction
    // `unappliedAddRequests` takes with the identical signal.
    guard let roster = snapshot.roster else { return .wait }

    let request = armed.request
    if request.needsRoleTable, snapshot.roleTable == nil { return .drop(.noRoleTable) }
    if alreadyApplied(request, roster: roster, roleTable: snapshot.roleTable) {
        return .drop(.alreadyApplied)
    }
    if alreadyWaiting(request, roster: roster, pendingKeys: snapshot.pendingKeys) {
        return .drop(.alreadyWaiting)
    }
    // ADR-0027 — LAST of the stopping conditions, so the two that describe the
    // world (the joiner landed / somebody's Add is waiting) are reported ahead
    // of the one that describes only our own core's bookkeeping. Both of those
    // also empty the quarantine eventually, and "they are already in" is the
    // more useful thing to have said.
    if case .approveJoin(let joinerKeyB64) = request {
        guard let retained = snapshot.retainedJoinKeys else { return .wait }
        if !retained.contains(joinerKeyB64) { return .drop(.joinRequestGone) }
    }
    return snapshot.pendingCount < armed.pendingCountAtRefusal ? .fire : .wait
}

/// Is the effect the request asked for already true of the group?
private func alreadyApplied(
    _ request: RetryableRequest,
    roster: [RetryRosterMember],
    roleTable: RetryRoleTable?
) -> Bool {
    func inRoster(_ keyB64: String) -> Bool {
        roster.contains { $0.signatureKeyB64 == keyB64 }
    }
    switch request {
    // An approve asks for exactly one thing — that this person be in the group
    // — so it is satisfied by them being in it, whoever's approve was swept.
    // Sharing the `add` arm is the statement, not a saving.
    case .add(let k, _), .approveJoin(let k):
        return inRoster(k)
    case .remove(let k), .leave(let k):
        return !inRoster(k)
    case .setAdmin(let k, let makeAdmin):
        guard let live = roleTable else { return false }
        // The ask is a PREDICATE about one member, not a whole table, so it is
        // satisfied by the live table saying what was asked however it got
        // there — including by somebody else having made the same change.
        return live.adminKeysB64.contains(k) == makeAdmin
    case .transferOwnership(let k, _):
        guard let live = roleTable else { return false }
        // The outgoing owner's own new role is deliberately NOT part of the
        // test. The ask that matters — and the only half a peer could observe
        // — is who holds the group.
        return live.ownerKeyB64 == k
    }
}

/// Is this exact request already in the live pending set?
///
/// The pending set keys a removal on a LEAF (``proposalKey(_:)`` has nothing
/// else to key it on), while a retry record keys it on a signature key — so the
/// leaf is resolved through the live roster here rather than stored, which is
/// what keeps a vacated-and-reused leaf from making an unrelated pending
/// removal look like ours.
private func alreadyWaiting(
    _ request: RetryableRequest,
    roster: [RetryRosterMember],
    pendingKeys: [String]
) -> Bool {
    let pending = Set(pendingKeys)
    switch request {
    // ADR-0045 S7 — an approved external join IS an ordinary admin-authored
    // `Add` carrying the joiner's retained KeyPackage, so it lands in the
    // pending set under exactly the key an add of the same person would. That
    // is the point of S7 (indistinguishable from an ordinary add on the wire)
    // and it is why the same lookup answers for both.
    case .add(let k, _), .approveJoin(let k):
        return pending.contains("add:\(k)")
    case .remove(let k), .leave(let k):
        guard let leaf = roster.first(where: { $0.signatureKeyB64 == k })?.leafIndex else {
            return false
        }
        return pending.contains("remove:\(leaf)") || pending.contains("leave:\(leaf)")
    case .setAdmin, .transferOwnership:
        return pending.contains("role-change")
    }
}

// MARK: - The store

/// Armed retries, per `(account, group)` scope key, per request key.
///
/// In memory on purpose — see the header. Nothing here touches disk.
///
/// The lock is explicit rather than an `@MainActor` annotation, for the reason
/// the header gives: `@MainActor` mutual exclusion across this claim would be
/// incidental, and one added suspension point would reopen the window
/// silently.
public final class MembershipRetryStore: @unchecked Sendable {
    private let lock = NSLock()
    private var armedByScope: [String: [String: ArmedRetry]] = [:]

    public init() {}

    /// The scope key. Both halves are required: an account can be a member of
    /// a group on two devices, and a device can hold two accounts.
    public static func scopeKey(accountIdB64: String, groupIdHex: String) -> String {
        "\(accountIdB64) \(groupIdHex)"
    }

    /// Arm (or re-arm) one request. At most one record per
    /// ``RetryableRequest/key``.
    public func arm(scope: String, _ armed: ArmedRetry) {
        lock.lock()
        defer { lock.unlock() }
        armedByScope[scope, default: [:]][armed.request.key] = armed
    }

    /// Take the record for `key` out of the store and return it, or `nil`.
    ///
    /// **This is the exactly-once primitive.** Both the retry driver and the
    /// manual handler call it, so whichever runs first owns the request and
    /// the other finds nothing. Deliberately not a read followed by a removal
    /// at some later suspension point: that is the shape that lets two
    /// evaluations both decide to fire.
    public func claim(scope: String, key: String) -> ArmedRetry? {
        lock.lock()
        defer { lock.unlock() }
        guard var bucket = armedByScope[scope], let found = bucket[key] else { return nil }
        bucket.removeValue(forKey: key)
        if bucket.isEmpty {
            armedByScope.removeValue(forKey: scope)
        } else {
            armedByScope[scope] = bucket
        }
        return found
    }

    /// Every armed record in `scope`, for the driver to evaluate. Never
    /// mutates — the driver claims what it decides to act on.
    public func armed(scope: String) -> [ArmedRetry] {
        lock.lock()
        defer { lock.unlock() }
        return Array((armedByScope[scope] ?? [:]).values)
    }

    /// Forget everything for one group — on leaving it, on sign-out.
    public func clear(scope: String) {
        lock.lock()
        defer { lock.unlock() }
        armedByScope.removeValue(forKey: scope)
    }
}
