// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0045 S4a — the pending-membership-request surface, as
/// `docs/ux/proposal-based-membership.md` specifies it. iOS mirror of the web
/// reference `apps/web/src/chat/pending-requests.ts`.
///
/// ## The three UX rules this module exists to enforce
///
/// 1. **No optimistic update.** A member with a pending removal STAYS in the
///    roster, annotated. Dropping the row would assert they are gone while they
///    are still cryptographically in the group — still receiving every message,
///    still able to decrypt — until the commit lands. That is the membership
///    equivalent of a green E2EE pill over an unencrypted call, and Lakko's
///    honesty corollary to invariant #4 forbids it. The same rule in reverse
///    governs a pending add: a person who has been *requested* is not a member
///    and must not appear in the members list, so they get their own section.
/// 2. **No `Cancel`.** Once published, the request is in every member's proposal
///    store and dies only when some commit lands. OpenMLS does expose a *local*
///    single-proposal eviction, but there is no **wire** withdraw — evicting
///    locally would leave the request live in every peer's store, which is a
///    worse lie than showing it. So the confirmation dialog is the last exit,
///    and this module deliberately exposes no withdraw verb.
/// 3. **No pending affordance until the quiet window elapses.** The owner is
///    rank 0 and sweeps its own request immediately, so in the common case the
///    request lands before anyone could look. A spinner that appears and
///    vanishes in 300 ms is worse than no spinner: it teaches the user that
///    membership is slow when it is not.
///
/// ## What is the source of truth, and what this store adds
///
/// `group.pendingProposals()` is the truth about *what* is pending — it is the
/// core's own retained set, populated on every member by the receipt gate, and
/// it is emptied by OpenMLS on every merged commit, so it needs no GC of its
/// own. This module never second-guesses it.
///
/// What the core cannot supply is *when* a request arrived, because a
/// `ProposalRecord` carries no timestamp. The quiet window needs one, and per
/// the UX spec it must be the **DS row timestamp**, never a local clock: every
/// member reads the identical value for the identical row, whereas a local
/// `Date()` would make the window mean something different on each device. So
/// this module keeps a small per-`(account, group)` observation store keyed on a
/// synthetic request key, and joins it to the live pending set.
///
/// Entries are pruned against the live set on every fold, so a swept or
/// superseded request cannot leave residue behind.

/// How a proposal is classified for the panel.
public enum PendingRequestKind: String, Equatable {
    case add
    case remove
    case leave
    case update
    case roleChange
    case unsupported
}

/// One pending request, joined from the live core set and the observation store.
public struct PendingRequest: Equatable, Identifiable {
    /// Stable synthetic key — see ``proposalKey(_:)``.
    public var key: String
    public var kind: PendingRequestKind
    /// base64 of the prospective member's signature key. `add` only.
    public var addSignatureKeyB64: String?
    /// The leaf this request removes. `remove` / `leave` only.
    public var targetLeaf: UInt32?
    /// The proposing member's leaf, or `nil` for an unresolvable sender.
    public var senderLeaf: UInt32?
    /// DS row timestamp in ms, or `nil` when this device never observed the row
    /// (it restored the proposal from a snapshot rather than seeing it land).
    public var observedAtMs: Int64?
    /// `true` when this device published it. Drives the UX §10 exception — a
    /// member always sees the status of a request they made themselves.
    public var own: Bool
    /// ADR-0045 UX §6.1.10 — base64 of the successor's signature key, on OUR OWN
    /// record only, for a `roleChange` that is an ownership transfer.
    ///
    /// It does two jobs at once, and neither is available any other way. It is
    /// the only thing that distinguishes *our pending role change is a transfer*
    /// from *our pending role change is a promote* — `ProposalKind.roleChange`
    /// carries no payload, so the core cannot tell them apart and
    /// ``proposalKey(_:)`` collapses every role change to the single key
    /// `role-change` — and it is what names the person in
    /// `Waiting to make {name} the owner.`
    ///
    /// The KEY is stored, never a label: the label is resolved through the LIVE
    /// roster at render time, so a successor who has since left degrades to the
    /// unnamed variant rather than to a stale name.
    ///
    /// Defaulted so the memberwise initializer stays callable without it: every
    /// other kind of request has no successor, and a test that is not about
    /// transfer should not have to say so.
    public var transferToB64: String? = nil
    /// `.pending` below the quiet window (render nothing), `.waiting` above it.
    public var state: PendingRequestState

    public var id: String { key }
}

/// Whether a request has been outstanding long enough to be worth showing.
public enum PendingRequestState: String, Equatable {
    case pending
    case waiting
}

/// How long a request must be outstanding before any persistent, group-visible
/// pending affordance renders.
///
/// **CHOSEN, not measured** — the UX spec leaves `QUIET_WINDOW` open (§12.1) and
/// this module is not in a position to close it, so the number is stated as a
/// judgement rather than dressed up as a finding. The budget it is sized
/// against: the owner's own sweep is one local stage plus two DS round-trips, so
/// a window shorter than that would flash an annotation on the path the UX spec
/// is explicit must look instantaneous; and a window much longer than a few
/// seconds would leave a genuine wait invisible for no gain. Nothing else in the
/// system depends on the value — it decides only *when* an annotation appears,
/// never whether a request is live — so it can be re-tuned from a measurement
/// without touching any other rule.
public let pendingQuietWindowMs: Int64 = 3000

/// A stable key for one request, so an observation recorded when the row landed
/// can be re-joined to the core's pending set on a later render.
///
/// Derived from what the request *is*, never from a leaf index alone where a
/// signature key is available: OpenMLS reuses a vacated leaf for the next Add,
/// so a leaf-keyed record can silently come to name someone else. For a removal
/// only the leaf is available (the core's `ProposalKind.remove` carries no
/// signature key), which is why the fold resolves it against the live roster at
/// render time rather than trusting a stored label.
///
/// `roleChange` collapses to a single key on purpose: `classify` discards the
/// `GroupContextExtensions` payload, so a received role-change request is only
/// describable at group level (UX §11.5) and two of them are indistinguishable.
public func proposalKey(_ record: ProposalRecord) -> String {
    switch record.kind {
    case .add(let signatureKey):
        return "add:\(base64Encode(signatureKey))"
    case .remove(let targetLeaf):
        return "remove:\(targetLeaf)"
    case .leave:
        return "leave:\(record.senderLeaf.map(String.init) ?? "?")"
    case .roleChange:
        return "role-change"
    case .update:
        return "update:\(record.senderLeaf.map(String.init) ?? "?")"
    case .unsupported:
        return "unsupported:\(record.senderLeaf.map(String.init) ?? "?")"
    }
}

/// The quiet-window rule.
///
/// An UNOBSERVED request (no DS timestamp — restored from a snapshot rather than
/// seen to land) renders as `waiting` immediately. That is the deliberate
/// direction: it has necessarily been pending since before this session started,
/// so it is by definition past any window, and treating it as `pending` would
/// hide a genuinely old request behind a timer that never fires.
public func requestState(
    observedAtMs: Int64?,
    nowMs: Int64,
    quietWindowMs: Int64 = pendingQuietWindowMs
) -> PendingRequestState {
    guard let observedAtMs else { return .waiting }
    return nowMs - observedAtMs >= quietWindowMs ? .waiting : .pending
}

/// One recorded observation of a proposal landing.
public struct RequestObservation: Equatable {
    /// DS row timestamp, ms since epoch.
    public var atMs: Int64
    /// `true` when this device published the request.
    public var own: Bool
    /// ADR-0045 UX §6.1.10 — see ``PendingRequest/transferToB64``.
    ///
    /// It lives HERE, on the observation record, rather than in a store of its
    /// own, and that is a correctness choice rather than tidiness: this store is
    /// pruned against the core's live pending set on every fold
    /// (``PendingRequestStore/prune(accountIdB64:groupIdB64:liveKeys:)``), so a
    /// swept or superseded transfer takes its successor key with it. A separate
    /// store would have to re-derive that prune, and a stale key surviving it
    /// would make an unrelated LATER role change render
    /// `Waiting to make {name} the owner.` about somebody nobody proposed.
    public var transferToB64: String?
    /// ADR-0045 S7 residual 1 — `true` when OUR OWN `add` request was published
    /// by approving an external **join** rather than by adding a contact.
    ///
    /// It lives here for the same reason ``transferToB64`` does: this store is
    /// pruned against the core's live pending set, so the flag dies with the
    /// request it describes. It is knowable only on the device that made the
    /// request, because on the wire an approved add is indistinguishable from an
    /// admin adding a contact (ADR-0045 S7) — and that device is the one
    /// `UnappliedRequests.swift` shows the notice on.
    public var joinRequest: Bool

    public init(
        atMs: Int64, own: Bool, transferToB64: String? = nil, joinRequest: Bool = false
    ) {
        self.atMs = atMs
        self.own = own
        self.transferToB64 = transferToB64
        self.joinRequest = joinRequest
    }
}

/// Join the core's live pending set to the observation store.
///
/// A record with `stored == false` is EXCLUDED: the receipt gate refused it, so
/// no sweep will ever commit it and it is not a pending request. That is a
/// refusal reported as data rather than as a throw, because a proposal advances
/// no state — dropping it unstored *is* the refusal.
///
/// Returns the requests in the core's own order plus the keys that are still
/// live, so the caller can prune the store in the same pass.
public func foldPendingRequests(
    records: [ProposalRecord],
    observations: [String: RequestObservation],
    nowMs: Int64,
    quietWindowMs: Int64 = pendingQuietWindowMs
) -> (requests: [PendingRequest], liveKeys: [String]) {
    var requests: [PendingRequest] = []
    var liveKeys: [String] = []
    for record in records {
        guard record.stored else { continue }
        let key = proposalKey(record)
        liveKeys.append(key)
        let seen = observations[key]
        requests.append(
            PendingRequest(
                key: key,
                kind: normaliseKind(record.kind),
                addSignatureKeyB64: addSignatureKeyB64(record.kind),
                targetLeaf: targetLeaf(of: record),
                senderLeaf: record.senderLeaf,
                observedAtMs: seen?.atMs,
                own: seen?.own ?? false,
                // Only meaningful on our own record, and only for a role change
                // — which is exactly where it is ever written.
                transferToB64: seen?.transferToB64,
                state: requestState(
                    observedAtMs: seen?.atMs, nowMs: nowMs, quietWindowMs: quietWindowMs)
            )
        )
    }
    return (requests, liveKeys)
}

/// The panel's visibility gate (UX §10).
///
/// > The pending set is visible to the owner and admins. Every member
/// > additionally sees the status of a request they made themselves. No member
/// > is ever told about a pending change to another member's status unless they
/// > can act on it.
///
/// The pending set is a work queue and only owner/admins have a verb over it.
/// Showing a plain member "Bob is waiting to be removed" tells them something
/// they cannot act on, about someone else's standing, in a window that did not
/// exist before ADR-0045 created it — and in a group where a removal is a safety
/// action, advertising it is a hazard.
///
/// Note what this is NOT: it is not concealment from the target. The receipt
/// gate stores the proposal on **every** member's device including theirs, so a
/// determined target can see it regardless. The choice is only whether to build
/// a surface that broadcasts it. Not rendering something asserts nothing, so it
/// breaks no honesty rule, and it matches what the members sheet already does —
/// `Add member`, `Remove`, the invite block and the join-request list are all
/// role-gated today.
///
/// The own-request exception is non-negotiable: without it a self-remove is an
/// action with no feedback.
public func visibleRequests(
    _ requests: [PendingRequest],
    canManageMembers: Bool
) -> [PendingRequest] {
    canManageMembers ? requests : requests.filter { $0.own }
}

/// The exact §5.1 pending-notice copy, or `nil` when nothing is `waiting`.
public struct PendingNotice: Equatable {
    public var count: String
    public var liveness: String
}

public func pendingNotice(_ requests: [PendingRequest]) -> PendingNotice? {
    let waiting = requests.filter { $0.state == .waiting }.count
    if waiting == 0 { return nil }
    return PendingNotice(
        count: Localization.text("chat.members.pending.count", counts: ["count": UInt64(waiting)]),
        // ONE line, not two. The escalated variant went with the `stalled`
        // state: ADR-0045 decision 2 gives a waiting request no deadline, so
        // there is no moment at which the client learns something new to say.
        // This sentence is true at ten seconds and at ten days, it states
        // INACTION (the only thing the client can observe), it names no person,
        // and it blames nobody.
        liveness: Localization.text("chat.members.pending.liveness")
    )
}

/// The row annotation for a member, or `nil` when that member has no pending
/// request against them.
///
/// `Waiting to leave.` when the request targets the reader's own leaf — the
/// self-remove that is how leaving works — otherwise `Waiting to be removed.`
/// A pending role change deliberately gets no row annotation: it carries no
/// target, so it cannot be attributed to a member (UX §11.5); it contributes to
/// the notice count and shows only at panel level.
public func rowAnnotation(
    _ requests: [PendingRequest],
    leafIndex: UInt32,
    ownLeaf: UInt32?
) -> String? {
    let hit = requests.first {
        $0.state == .waiting
            && ($0.kind == .remove || $0.kind == .leave)
            && $0.targetLeaf == leafIndex
    }
    guard hit != nil else { return nil }
    return Localization.text(
        leafIndex == ownLeaf
            ? "chat.members.pending.waiting_leave" : "chat.members.pending.waiting_removed")
}

/// The `Waiting to be added` rows — pending adds only, past the window.
/// ADR-0045 S7 residual 2 — the queue an approve just grew, named to the person
/// who grew it.
///
/// ## Why this line, and why only this line
///
/// Approving a join no longer admits anyone: it publishes an `Add` proposal into
/// the member pending set, and that set is drained only by a sweep, which
/// ADR-0045 decision 2 gives no deadline. So an admin can approve into a queue
/// indefinitely, and before this the only feedback in the whole system was the
/// core's cap refusal — which lands on **whoever proposes next**, who is very
/// often somebody else entirely.
///
/// This does not bound anything and does not pretend to. Bounding needs a number
/// this layer does not have: `MAX_PENDING_PROPOSALS` is `pub(crate)` in
/// `lakko-mls` and is on neither FFI surface, and a client-side literal `64`
/// would be exactly the stale-literal drift class this repo already names as
/// failing in the dangerous direction. What it does is make the accumulation
/// *visible at the moment it is caused*, which is the part a client can do
/// honestly.
///
/// `nil` below two, because "1 change is now waiting" after making one change is
/// noise rather than information.
public func waitingQueueLine(_ pendingCount: Int) -> String? {
    pendingCount < 2
        ? nil
        : Localization.text("chat.members.pending.queue", counts: ["count": UInt64(pendingCount)])
}

// The "Join requests are not saved" note that used to live here is gone with
// the reason for it: since 2026-09-26 `Group::pending_external_joins` rides the
// MLS storage snapshot (ADR-0027 update note), so a restart shows the same
// list. Rows are still never aged out, which would hide something still live
// for the joiner.

public func waitingToBeAdded(_ requests: [PendingRequest]) -> [PendingRequest] {
    requests.filter { $0.kind == .add && $0.state == .waiting }
}

// MARK: - The observation store

/// Per-`(account, group)` record of `proposalKey -> RequestObservation`.
///
/// `UserDefaults`-backed for the same reason as `BeaconObservationStore`: this
/// is timing metadata about a proposal that already rides the journal in
/// plaintext-to-members form, and losing it degrades the quiet window to "render
/// immediately" rather than losing the request.
public final class PendingRequestStore {

    private static let prefix = "lakko.pending-requests.v1"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func storeKey(accountIdB64: String, groupIdB64: String) -> String {
        "\(Self.prefix).\(accountIdB64).\(groupIdB64)"
    }

    /// Read the observations for one group. Never throws: a corrupt or absent
    /// row reads as empty, which degrades the quiet window to "render
    /// immediately" rather than losing the request. The request itself lives in
    /// the core's pending set, so nothing here is load-bearing for correctness —
    /// only for *when* an annotation appears.
    public func load(accountIdB64: String, groupIdB64: String) -> [String: RequestObservation] {
        guard let raw = defaults.dictionary(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        ) else {
            return [:]
        }
        var out: [String: RequestObservation] = [:]
        for (key, value) in raw {
            guard let pair = value as? [String: Any],
                  let atMs = (pair["atMs"] as? NSNumber)?.int64Value,
                  let own = (pair["own"] as? NSNumber)?.boolValue else { continue }
            out[key] = RequestObservation(
                atMs: atMs,
                own: own,
                // ADR-0045 §6.1.10 — preserved through the round-trip, and
                // validated the same way as the rest: anything that is not a
                // string is simply absent, which degrades a pending transfer to
                // its unnamed variant rather than losing the request.
                transferToB64: pair["transferTo"] as? String,
                // ADR-0045 S7 residual 1. Absent reads as an ordinary add, which
                // is the copy that offers the weaker action rather than the
                // stronger claim.
                joinRequest: (pair["join"] as? NSNumber)?.boolValue ?? false)
        }
        return out
    }

    private func save(
        accountIdB64: String,
        groupIdB64: String,
        _ observations: [String: RequestObservation]
    ) {
        var raw: [String: [String: Any]] = [:]
        for (key, value) in observations {
            var entry: [String: Any] = [
                "atMs": NSNumber(value: value.atMs),
                "own": NSNumber(value: value.own),
            ]
            // Written only when present, so an ordinary add / remove / promote
            // observation is the same three-key plist row it always was.
            if let transferTo = value.transferToB64 { entry["transferTo"] = transferTo }
            if value.joinRequest { entry["join"] = NSNumber(value: true) }
            raw[key] = entry
        }
        defaults.set(raw, forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// Record that a proposal landed, keyed on ``proposalKey(_:)``.
    ///
    /// The FIRST observation of a key wins: a re-broadcast or a re-request of
    /// the same request must not reset its age, or a request that keeps being
    /// re-sent would never mature past the quiet window and would stay invisible
    /// forever — exactly the wait the notice exists to surface. `own` is
    /// upgraded to `true` if this device later publishes the same request,
    /// because the own-request visibility exception must not be lost to arrival
    /// order.
    ///
    /// `transferToB64` (ADR-0045 §6.1.10) follows the same first-wins rule for
    /// the same reason: the successor belongs to the request that is actually
    /// pending, and only the device that published it can supply one. A later
    /// observation of the same key without one must not erase it — the receive
    /// loop observes its own request landing back off the journal, and that
    /// observation carries no successor.
    public func record(
        accountIdB64: String,
        groupIdB64: String,
        key: String,
        atMs: Int64,
        own: Bool,
        transferToB64: String? = nil,
        joinRequest: Bool = false
    ) {
        var observations = load(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        let existing = observations[key]
        observations[key] = RequestObservation(
            atMs: existing?.atMs ?? atMs,
            own: (existing?.own ?? false) || own,
            transferToB64: existing?.transferToB64 ?? transferToB64,
            // Sticky-true, for the same reason `own` is: the receive loop
            // observes our own approval landing back off the journal and that
            // observation knows nothing about where the request came from.
            // Losing the flag to arrival order would silently downgrade the copy
            // to the wrong action.
            joinRequest: (existing?.joinRequest ?? false) || joinRequest
        )
        save(accountIdB64: accountIdB64, groupIdB64: groupIdB64, observations)
    }

    /// Drop every observation whose request is no longer in the core's pending
    /// set — the request was swept, superseded, or the group moved.
    public func prune(accountIdB64: String, groupIdB64: String, liveKeys: [String]) {
        var observations = load(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        let live = Set(liveKeys)
        let before = observations.count
        observations = observations.filter { live.contains($0.key) }
        if observations.count != before {
            save(accountIdB64: accountIdB64, groupIdB64: groupIdB64, observations)
        }
    }

    /// Drop every observation for one account — `recover` / `forgetDevice`
    /// hygiene, the same discipline the commit outbox follows.
    public func clearForAccount(accountIdB64: String) {
        let needle = "\(Self.prefix).\(accountIdB64)."
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(needle) {
            defaults.removeObject(forKey: key)
        }
    }
}

// MARK: - Internals

private func normaliseKind(_ kind: ProposalKind) -> PendingRequestKind {
    switch kind {
    case .add: return .add
    case .remove: return .remove
    case .leave: return .leave
    case .update: return .update
    case .roleChange: return .roleChange
    case .unsupported: return .unsupported
    }
}

private func addSignatureKeyB64(_ kind: ProposalKind) -> String? {
    if case .add(let signatureKey) = kind { return base64Encode(signatureKey) }
    return nil
}

/// Which leaf a request is *about*, for the row annotation.
///
/// A `remove` names its target explicitly. A `leave` does not, and the web
/// reference reads `removeTargetLeaf` for it too — which is always absent, so
/// its `Waiting to leave.` branch is unreachable there. It is reachable here
/// because `ProposalKind.leave` "structurally targets the sender's own leaf and
/// can name no other" (the FFI's own words), so the sender leaf IS the target.
/// Nothing else in the fold changes; this only makes the annotation the UX spec
/// specifies actually render.
private func targetLeaf(of record: ProposalRecord) -> UInt32? {
    switch record.kind {
    case .remove(let targetLeaf): return targetLeaf
    case .leave: return record.senderLeaf
    default: return nil
    }
}
