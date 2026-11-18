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
/// here may claim one.
///
/// ## Since ADR-0050 section 2.5a: re-requested first, listed only when that stops
///
/// The add-only detector that used to live here (an own `add` observation gone
/// from the pending set and absent from the roster) is GONE. A plain member
/// may now sweep device admissions, and its commit drops every other pending
/// proposal by construction, so a dropped request stopped being an edge case.
/// Every own request now records its INTENT when it is proposed
/// (``MembershipIntent``, in `PendingRequests.swift`), and after each merged
/// commit the repository re-requests whatever that commit superseded —
/// silently, re-derived against the current epoch (`MembershipRetry.swift`,
/// ``supersededVerdict(_:_:)``). A request reaches this list only when the
/// re-request STOPS for a reason other than *landed* or *its subject left*:
/// the authority it needs is gone (a typed refusal with disposition
/// `NotPermitted` or `DoNotRetry`), this device's membership ended, or an
/// approved join whose material only the joiner holds.
///
/// Because an intent names its subject by a signature key rather than a leaf,
/// a removal, a role change and a leave can be listed too, which the old
/// detector could not do honestly (a `remove:` observation names a leaf, and a
/// role-change observation names nothing).
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
/// deadline. So the joiner gets no notice; it gets the **repair** — after a
/// day with no answer, `Ask again` on the `/g/` landing screen and in
/// Requests sent (`RequestsSentStore.mayAskAgain`), user-driven and never
/// automatic, which re-mints a fresh KeyPackage.

/// What kind of own request a `Not applied` row is about.
///
/// `add` is the original (ADR-0045 S7 residual 1) row; the rest arrived with
/// ADR-0050 section 2.5a, whose superseded re-request lists a request here
/// when it STOPS for a reason other than *its effect landed* or *its subject
/// left*: this device no longer holds the authority the request needs, its
/// own membership ended, or (for an approved join) the material to re-approve
/// is gone.
public enum UnappliedKind: String, Equatable {
    case add
    case removal
    case makeAdmin = "make-admin"
    case removeAdmin = "remove-admin"
    case ownershipTransfer = "transfer"
    case leave
    case deviceAdd = "device-add"
    case deviceRemoval = "device-remove"
}

/// One own request that will not apply.
///
/// The name is historical (the row was add-only until ADR-0050 2.5a);
/// ``kind`` says what it is about.
public struct UnappliedAdd: Equatable {
    /// base64 of the 32-byte Ed25519 key the request is ABOUT: the person's
    /// account key for an add / removal / role change / leave, the joiner's
    /// leaf key for an approved join, the device's leaf key for a device row.
    public let addSignatureKeyB64: String
    /// `true` when this device published the request by approving an external
    /// **join** rather than by adding a contact.
    ///
    /// Knowable ONLY on the device that made the request: on the wire an
    /// approved add is indistinguishable from an admin adding a contact
    /// (ADR-0045 S7), which is the point rather than a limitation. That device
    /// is the one this notice is shown on.
    public let viaJoinRequest: Bool
    public let kind: UnappliedKind

    public init(addSignatureKeyB64: String, viaJoinRequest: Bool, kind: UnappliedKind = .add) {
        self.addSignatureKeyB64 = addSignatureKeyB64
        self.viaJoinRequest = viaJoinRequest
        self.kind = kind
    }

    /// The store key and the list identity. An add row keeps the bare key it
    /// always had; every other kind is prefixed, so a removal and a role change
    /// about the same person are two rows rather than one overwriting the
    /// other.
    public var rowKey: String {
        kind == .add ? addSignatureKeyB64 : "\(kind.rawValue):\(addSignatureKeyB64)"
    }
}

/// The `Not applied` row for an own request whose re-request stopped. Every
/// intent kind maps to exactly one row kind.
public func unappliedRow(for intent: MembershipIntent) -> UnappliedAdd {
    switch intent.request {
    case .add(let person, _, _):
        return UnappliedAdd(addSignatureKeyB64: person, viaJoinRequest: false, kind: .add)
    case .approveJoin(let joiner):
        return UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true, kind: .add)
    case .remove(let person):
        return UnappliedAdd(addSignatureKeyB64: person, viaJoinRequest: false, kind: .removal)
    case .setAdmin(let person, let makeAdmin):
        return UnappliedAdd(
            addSignatureKeyB64: person, viaJoinRequest: false,
            kind: makeAdmin ? .makeAdmin : .removeAdmin)
    case .transferOwnership(let to, _):
        return UnappliedAdd(
            addSignatureKeyB64: to, viaJoinRequest: false, kind: .ownershipTransfer)
    case .leave(let own):
        return UnappliedAdd(addSignatureKeyB64: own, viaJoinRequest: false, kind: .leave)
    case .ownDeviceAdd(let key):
        return UnappliedAdd(addSignatureKeyB64: key, viaJoinRequest: false, kind: .deviceAdd)
    case .ownDeviceRemove(let key):
        return UnappliedAdd(addSignatureKeyB64: key, viaJoinRequest: false, kind: .deviceRemoval)
    }
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
/// the action to them. An add is re-doable here when this device may add.
///
/// The kinds ADR-0050 2.5a added carry no advice at all: such a row exists
/// because a re-request STOPPED, usually because this device no longer holds
/// the authority the request needs, so "you can do it again" would be false
/// in the common case. They say what did not happen and nothing more.
/// The lines are catalog keys (ADR-0061), one per kind, shared with web and
/// Android.
public func unappliedLine(_ item: UnappliedAdd) -> String {
    switch item.kind {
    case .add:
        return item.viaJoinRequest
            ? Localization.text("chat.members.unapplied.join_line")
            : Localization.text("chat.members.unapplied.add_line")
    case .removal: return Localization.text("chat.members.unapplied.remove_line")
    case .makeAdmin: return Localization.text("chat.members.unapplied.make_admin_line")
    case .removeAdmin: return Localization.text("chat.members.unapplied.remove_admin_line")
    case .ownershipTransfer: return Localization.text("chat.members.unapplied.transfer_line")
    case .leave: return Localization.text("chat.members.unapplied.leave_line")
    case .deviceAdd: return Localization.text("chat.members.unapplied.admit_device_line")
    case .deviceRemoval: return Localization.text("chat.members.unapplied.retire_device_line")
    }
}

/// The one explanatory line under the section heading. True of every row: a
/// row is listed only once nothing will send the request again — the ADR-0050
/// 2.5a re-request stopped for it (UX §7's automatic re-request of a
/// superseded request runs silently and lists nothing while it is still
/// trying).
public var unappliedExplanation: String { Localization.text("chat.members.unapplied.explanation") }

/// The section heading.
public func unappliedHeading(_ count: Int) -> String {
    Localization.text("chat.members.unapplied.heading", texts: ["count": count.formatted(.number.locale(Localization.currentLocale))])
}

// MARK: - The joiner's half of the same residual

/// What the `/g/` landing screen says while a request is outstanding and
/// under a day old (`docs/ux/group-invite-page.md`, "After `Request to
/// join`"). The joiner is told **nothing** about an exclusion, for the
/// reasons in this file's header. There is no control beside it: after a day
/// the page says "No answer yet." and offers `Ask again`
/// (`requests_sent.no_answer`, `requests_sent.ask_again`), the same request
/// the Requests sent list sends.
public var joinWaitPrimary: String { Localization.text("group.join.wait_primary") }

// MARK: - The store

/// One stored row. `dismissed` is a tombstone: without it a dismissed row
/// that is detected again (a re-request that stops again) would reappear, and
/// the notice could not be dismissed at all.
public struct UnappliedRow: Equatable {
    public let viaJoinRequest: Bool
    public let dismissed: Bool
    /// The subject key and the kind. Absent in a row written before ADR-0050
    /// 2.5a, which is an add row keyed on its bare subject.
    public let subjectKeyB64: String?
    public let kind: UnappliedKind

    public init(
        viaJoinRequest: Bool, dismissed: Bool = false,
        subjectKeyB64: String? = nil, kind: UnappliedKind = .add
    ) {
        self.viaJoinRequest = viaJoinRequest
        self.dismissed = dismissed
        self.subjectKeyB64 = subjectKeyB64
        self.kind = kind
    }
}

/// Per-`(account, group)` record of ``UnappliedAdd/rowKey`` -> ``UnappliedRow``.
///
/// `UserDefaults`-backed for the same reason as ``PendingRequestStore``: it
/// holds a signature key that already rides the journal in plaintext-to-members
/// form plus two booleans and a kind — no key material — and losing it costs a
/// notice rather than a request.
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
                dismissed: (pair["dismissed"] as? NSNumber)?.boolValue ?? false,
                subjectKeyB64: pair["subject"] as? String,
                kind: (pair["kind"] as? String).flatMap(UnappliedKind.init(rawValue:)) ?? .add)
        }
        return out
    }

    private func saveRows(
        accountIdB64: String, groupIdB64: String, _ rows: [String: UnappliedRow]
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var raw: [String: [String: Any]] = [:]
        for (key, row) in rows {
            var entry: [String: Any] = ["join": NSNumber(value: row.viaJoinRequest)]
            if row.dismissed { entry["dismissed"] = NSNumber(value: true) }
            if let subject = row.subjectKeyB64 { entry["subject"] = subject }
            if row.kind != .add { entry["kind"] = row.kind.rawValue }
            raw[key] = entry
        }
        defaults.set(raw, forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// The rows still worth showing, in a stable order.
    public func load(accountIdB64: String, groupIdB64: String) -> [UnappliedAdd] {
        loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            .filter { !$0.value.dismissed }
            .map {
                UnappliedAdd(
                    addSignatureKeyB64: $0.value.subjectKeyB64 ?? $0.key,
                    viaJoinRequest: $0.value.viaJoinRequest,
                    kind: $0.value.kind)
            }
            .sorted { $0.rowKey < $1.rowKey }
    }

    /// Record detected rows. Idempotent, and a **dismissed row is never
    /// resurrected**.
    public func record(accountIdB64: String, groupIdB64: String, items: [UnappliedAdd]) {
        guard !items.isEmpty else { return }
        var rows = loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        var changed = false
        for item in items where rows[item.rowKey] == nil {
            rows[item.rowKey] = UnappliedRow(
                viaJoinRequest: item.viaJoinRequest,
                subjectKeyB64: item.kind == .add ? nil : item.addSignatureKeyB64,
                kind: item.kind)
            changed = true
        }
        if changed { saveRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64, rows) }
    }

    /// Dismiss one row by its ``UnappliedAdd/rowKey`` (for an add row, its bare
    /// subject key — hence the parameter's historical name). It stays
    /// dismissed.
    public func dismiss(accountIdB64: String, groupIdB64: String, addSignatureKeyB64: String) {
        var rows = loadRows(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        guard let existing = rows[addSignatureKeyB64] else { return }
        rows[addSignatureKeyB64] = UnappliedRow(
            viaJoinRequest: existing.viaJoinRequest, dismissed: true,
            subjectKeyB64: existing.subjectKeyB64, kind: existing.kind)
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
