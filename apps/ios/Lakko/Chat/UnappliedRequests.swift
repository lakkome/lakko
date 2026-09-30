// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0045 S7 residual 1 — the request that was not carried. iOS mirror of the
/// web reference `apps/web/src/chat/unapplied-requests.ts`.
///
/// ## The gap
///
/// A sweep does not necessarily carry everything it finds. `sweep_keeps`
/// enumerates fifteen `CommitBuilder::build` conditions and
/// `narrow_sweep_by_exclusion` drops whatever still refuses to build — and the
/// commonest cause needs no adversary at all: a **KeyPackage that expired**,
/// reachable precisely because ADR-0045 decision 2 gives a request no deadline.
///
/// Before this file, an excluded request was simply *gone*. OpenMLS empties the
/// whole proposal store on every merged commit, so the pending set that was the
/// request's only representation is cleared whether or not the commit carried
/// it; `PendingRequestStore.prune` then removes the local observation; and the
/// panel falls silent. For an **approved join** it is worse than silent, because
/// a successful `approve_external_join` **consumes** the retained request, so
/// the quarantine entry is gone too and the admin cannot even re-approve.
///
/// ## What is knowable, and what is not
///
/// The core reports **no reason**: `CreateCommitError` carries no proposal
/// reference on any variant, so "why" does not exist at this layer and no copy
/// here may claim one. What exists is the observation:
///
/// > **A request of mine left the core's pending set without its effect landing.**
///
/// That runs on **every** device rather than only on the sweeper — which
/// matters, because the device that swept is often not the device that asked —
/// and it needs no core change, no new wire artifact and no new FFI.
///
/// ## Why only an `add`
///
/// `remove:` and `leave:` keys name a **leaf index**, and OpenMLS hands a
/// vacated leaf to the next `Add`, so a roster test on a leaf can be wrong in
/// both directions. `role-change` carries no payload at all. And all three are
/// already **visible** when they fail — the member you tried to remove is still
/// in the roster, the group you failed to leave is still in your list. The
/// `add` is the only request whose failure leaves no trace anywhere, and the
/// only one whose effect test is keyed on a **signature key** rather than a leaf.
///
/// ## What the JOINER is told: nothing, and that is a finding rather than a gap
///
/// A link joiner is a **non-member polling the group journal**. There is no
/// artifact addressed to it, and the journal is unauthenticated — the DS
/// pseudonym IS the `group_id` and there is no auth on it — so any "your request
/// was not carried" envelope would be a **forgeable denial primitive** rather
/// than a signal. Inference does not work either: an epoch advancing with no
/// rendezvous for us is byte-for-byte what *no admin has approved yet* looks
/// like, which is the ordinary state of a request that by design has no
/// deadline. So the joiner gets no notice; it gets the **repair** —
/// ``joinRequestAgain`` on the `/g/` landing screen, user-driven and never
/// automatic, which re-mints a fresh KeyPackage.

/// One own `add` request that left the pending set without landing.
public struct UnappliedAdd: Equatable {
    /// base64 of the prospective member's 32-byte Ed25519 signature key.
    public let addSignatureKeyB64: String
    /// `true` when this device published the request by approving an external
    /// **join** rather than by adding a contact.
    ///
    /// Knowable ONLY on the device that made the request: on the wire an
    /// approved add is indistinguishable from an admin adding a contact
    /// (ADR-0045 S7), which is the point rather than a limitation. That device
    /// is the one this notice is shown on.
    public let viaJoinRequest: Bool

    public init(addSignatureKeyB64: String, viaJoinRequest: Bool) {
        self.addSignatureKeyB64 = addSignatureKeyB64
        self.viaJoinRequest = viaJoinRequest
    }
}

/// The own `add` requests that are dead: observed as ours, no longer in the
/// core's live pending set, and not in the roster.
///
/// Pure. `liveKeys` and `rosterKeysB64` must be read in the SAME pass — the
/// pending set is emptied by the merge that also moves the roster.
///
/// A `nil` roster is a torn read, not an empty group, and claims NOTHING — the
/// same fail-closed direction ``membershipRotationDecision`` takes with the
/// identical signal.
public func unappliedAddRequests(
    observations: [String: RequestObservation],
    liveKeys: [String],
    rosterKeysB64: [String]?
) -> [UnappliedAdd] {
    guard let rosterKeysB64 else { return [] }
    let live = Set(liveKeys)
    let roster = Set(rosterKeysB64)
    var out: [UnappliedAdd] = []
    for (key, seen) in observations {
        // UX §10 — a member is never told about someone else's request.
        guard seen.own else { continue }
        guard key.hasPrefix("add:") else { continue }
        let addKeyB64 = String(key.dropFirst("add:".count))
        // `proposalKey` writes `add:` with an empty key when none was resolved.
        // That names nobody, so there is nothing honest to say about it.
        if addKeyB64.isEmpty || addKeyB64 == "?" { continue }
        if live.contains(key) { continue }
        if roster.contains(addKeyB64) { continue }
        out.append(UnappliedAdd(addSignatureKeyB64: addKeyB64, viaJoinRequest: seen.joinRequest))
    }
    // Dictionary iteration order is unspecified, so sort for a stable render.
    return out.sorted { $0.addSignatureKeyB64 < $1.addSignatureKeyB64 }
}

/// The line under one row.
///
/// States that it did not apply and what can be done, **never why** — the core
/// has no reason to give and inventing one would be the same class of lie as a
/// green E2EE pill over an unencrypted call. The UX spec §7 draft string
/// (`This request expired before it could be applied.`) asserts a cause this
/// client cannot establish; the divergence is deliberate.
///
/// A join approval is **terminal on the admin side** — the approve consumed the
/// retained request and only the joiner holds the key material — so it directs
/// the action to them. An ordinary add is re-doable here.
public func unappliedLine(_ item: UnappliedAdd) -> String {
    item.viaJoinRequest
        ? Localization.text("chat.members.unapplied.join_line")
        : Localization.text("chat.members.unapplied.add_line")
}

/// The one explanatory line under the section heading. True on both counts: the
/// request left the pending set, so no sweep will carry it, and no client
/// re-requests (UX §7's automatic re-request is designed and not built).
public var unappliedExplanation: String { Localization.text("chat.members.unapplied.explanation") }

/// The section heading.
public func unappliedHeading(_ count: Int) -> String {
    Localization.text("chat.members.unapplied.heading", texts: ["count": count.formatted(.number.locale(Localization.currentLocale))])
}

// MARK: - The joiner's half of the same residual

/// What the `/g/` landing screen says while a request is outstanding.
///
/// The joiner is told **nothing** about an exclusion, for the reasons in this
/// file's header. What it is told instead is the truth about the wait itself,
/// which the previous copy left out: **both** steps are unbounded.
public var joinWaitPrimary: String { Localization.text("group.join.wait_primary") }

/// The second line — the part that was missing, and the only thing that makes
/// the ``joinRequestAgain`` control legible.
public var joinWaitSecondary: String { Localization.text("group.join.wait_secondary") }

/// The joiner's one repair, and the ONLY thing available to this side.
///
/// A fresh request mints a fresh KeyPackage, which is what fixes the commonest
/// cause of an exclusion — but the button never says so, because this side
/// cannot know that was the cause. Deliberately **user-driven and never
/// automatic**: each press is a journal row held to the DS's 30-day TTL, and a
/// timer would spin against a group that is merely quiet.
///
/// Its reach is honest but partial: an admin still holding an earlier request
/// for the same signature key **keeps the older KeyPackage**
/// (`retain_external_join` de-dups by signature key and skips the push), so a
/// re-request refreshes the material only where the earlier entry is gone —
/// which is exactly the case that matters here, because a successful approve
/// consumed it.
public var joinRequestAgain: String { Localization.text("group.join.request_again") }

// MARK: - The store

/// One stored row. `dismissed` is a tombstone: the detection re-runs on every
/// fold, so without it the notice could not be dismissed at all.
public struct UnappliedRow: Equatable {
    public let viaJoinRequest: Bool
    public let dismissed: Bool

    public init(viaJoinRequest: Bool, dismissed: Bool = false) {
        self.viaJoinRequest = viaJoinRequest
        self.dismissed = dismissed
    }
}

/// Per-`(account, group)` record of `addSignatureKeyB64 -> UnappliedRow`.
///
/// `UserDefaults`-backed for the same reason as ``PendingRequestStore``: it
/// holds a signature key that already rides the journal in plaintext-to-members
/// form plus two booleans — no key material — and losing it costs a notice
/// rather than a request.
public final class UnappliedRequestStore {

    private static let prefix = "lakko.unapplied-requests.v1"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func storeKey(accountIdB64: String, groupIdB64: String) -> String {
        "\(Self.prefix).\(accountIdB64).\(groupIdB64)"
    }

    private func loadRows(accountIdB64: String, groupIdB64: String) -> [String: UnappliedRow] {
        guard let raw = defaults.dictionary(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        ) else { return [:] }
        var out: [String: UnappliedRow] = [:]
        for (key, value) in raw {
            guard let pair = value as? [String: Any],
                  let via = (pair["join"] as? NSNumber)?.boolValue else { continue }
            out[key] = UnappliedRow(
                viaJoinRequest: via,
                dismissed: (pair["dismissed"] as? NSNumber)?.boolValue ?? false)
        }
        return out
    }

    private func saveRows(
        accountIdB64: String, groupIdB64: String, _ rows: [String: UnappliedRow]
    ) {
        var raw: [String: [String: Any]] = [:]
        for (key, row) in rows {
            var entry: [String: Any] = ["join": NSNumber(value: row.viaJoinRequest)]
            if row.dismissed { entry["dismissed"] = NSNumber(value: true) }
            raw[key] = entry
        }
        defaults.set(raw, forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// The rows still worth showing, in a stable order.
    public func load(accountIdB64: String, groupIdB64: String) -> [UnappliedAdd] {
        loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            .filter { !$0.value.dismissed }
            .map { UnappliedAdd(addSignatureKeyB64: $0.key, viaJoinRequest: $0.value.viaJoinRequest) }
            .sorted { $0.addSignatureKeyB64 < $1.addSignatureKeyB64 }
    }

    /// Record detected rows. Idempotent, and a **dismissed row is never
    /// resurrected**.
    public func record(accountIdB64: String, groupIdB64: String, items: [UnappliedAdd]) {
        guard !items.isEmpty else { return }
        var rows = loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        var changed = false
        for item in items where rows[item.addSignatureKeyB64] == nil {
            rows[item.addSignatureKeyB64] = UnappliedRow(viaJoinRequest: item.viaJoinRequest)
            changed = true
        }
        if changed { saveRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64, rows) }
    }

    /// Dismiss one row. It stays dismissed.
    public func dismiss(accountIdB64: String, groupIdB64: String, addSignatureKeyB64: String) {
        var rows = loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        guard let existing = rows[addSignatureKeyB64] else { return }
        rows[addSignatureKeyB64] = UnappliedRow(
            viaJoinRequest: existing.viaJoinRequest, dismissed: true)
        saveRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64, rows)
    }

    /// Drop every row for one group — `Delete this chat` hygiene.
    public func clearForGroup(accountIdB64: String, groupIdB64: String) {
        defaults.removeObject(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// Drop every row for one account — `recover` / `forgetDevice` hygiene, the
    /// same discipline the observation store and the commit outbox follow.
    public func clearForAccount(accountIdB64: String) {
        let needle = "\(Self.prefix).\(accountIdB64)."
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(needle) {
            defaults.removeObject(forKey: key)
        }
    }
}
