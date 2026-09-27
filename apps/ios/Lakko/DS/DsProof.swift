// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0053 — a claimed journal on the Delivery Service is opened by a key, not
// by its name. This file holds the client-side pieces every DS caller shares:
// what signs a request, what a request carries for its 401 retry, the clock
// check that decides the copy of a final refusal, the previous-epoch signer
// memory, the knock-row filter and the link joiner's knock sequence.
//
// The canonical bytes and the signature are the Rust core's
// (`lakko-ds-proof`, reached through `DsProofSigner`); nothing here re-derives
// them. What lives here is only which signer, which inputs, and in which order.

/// Anything that can sign one DS request at one epoch.
///
/// The production value is the FFI `DsProofSigner` (a conversation group,
/// DeviceGroup or PublisherGroup signer, or a mailbox signer). A protocol only
/// so a test can hand the client a fake and read back the exact inputs it was
/// asked to sign.
public protocol DsRequestSigner: AnyObject {
    /// The epoch the signer proves membership at (`dsMailboxEpoch()` for a
    /// mailbox).
    func epoch() -> UInt64
    /// The `x-lakko-ds-proof` header value for one request. `path` is the
    /// SERVICE path (`/ds/group/<hex>/messages`), never the nginx prefix;
    /// `query` the raw query string without `?`; `body` the exact bytes sent.
    func sign(method: String, path: String, query: String, body: Data) -> String
}

/// A group-journal signer that can also produce its own registration body
/// (`groupClaimBodyJson()`) — what register-before-publish and heal-on-apply
/// need. The FFI `DsProofSigner` of a conversation group, DeviceGroup or
/// PublisherGroup.
public protocol DsGroupSigner: DsRequestSigner {
    /// The JSON body of `PUT /ds/group/<hex>/access` registering this signer's
    /// key at its epoch.
    func groupClaimBodyJson() -> String
}

extension DsProofSigner: DsGroupSigner {}

/// How a request to a claimed journal is signed, and what the client may do
/// once when the server refuses it (ADR-0053 client contract point 5).
public struct DsAuth {
    /// The signer of the journal at this device's current epoch.
    public var signer: DsRequestSigner
    /// The previous epoch's signer, when this device still holds it: the
    /// server may not yet have the current epoch registered (a lagging
    /// registration), and it still accepts the previous one.
    public var previous: DsRequestSigner?
    /// Claim the journal again (claim-on-open) and return the signer to retry
    /// with, or `nil` when there is nothing to retry with.
    public var reclaim: (() async -> DsRequestSigner?)?

    public init(
        signer: DsRequestSigner,
        previous: DsRequestSigner? = nil,
        reclaim: (() async -> DsRequestSigner?)? = nil
    ) {
        self.signer = signer
        self.previous = previous
        self.reclaim = reclaim
    }
}

/// ADR-0053 client contract point 3 — the shape of every direct
/// (eager-merge) commit: take the epoch-N signer BEFORE the create, create
/// (the group is now at N+1), take the N+1 signer, register it proven under N,
/// and only then record the ADR-0040 outbox entry and publish the commit
/// signed under N. A failed registration is handled exactly like a failed
/// outbox write: roll back, publish nothing.
///
/// So a commit is never on the wire before the server knows its epoch, and no
/// peer that applies it holds an epoch the server refuses.
public struct DirectCommitSteps<Created, Prepared, Published> {
    /// The group's signer at its CURRENT epoch.
    public var currentSigner: () throws -> DsGroupSigner
    /// Build the commit. Eager-merges N -> N+1 in memory.
    public var create: () throws -> Created
    /// `PUT access` for `next`, proven under `provenBy`.
    public var register: (_ next: DsGroupSigner, _ provenBy: DsGroupSigner) async throws -> Void
    /// Everything that must be durable before the publish (the second-artifact
    /// compute + the ADR-0040 entry + the N+1 snapshot). Runs only after a
    /// successful registration.
    public var prepare: (Created) throws -> Prepared
    /// Undo the eager merge (reset to the durable epoch-N snapshot). Called when
    /// the registration or the prepare throws.
    public var rollback: () throws -> Void
    /// Publish the commit (and whatever else), signed under the epoch-N signer.
    public var publish: (Prepared, DsGroupSigner) async throws -> Published

    public init(
        currentSigner: @escaping () throws -> DsGroupSigner,
        create: @escaping () throws -> Created,
        register: @escaping (_ next: DsGroupSigner, _ provenBy: DsGroupSigner) async throws -> Void,
        prepare: @escaping (Created) throws -> Prepared,
        rollback: @escaping () throws -> Void,
        publish: @escaping (Prepared, DsGroupSigner) async throws -> Published
    ) {
        self.currentSigner = currentSigner
        self.create = create
        self.register = register
        self.prepare = prepare
        self.rollback = rollback
        self.publish = publish
    }

    public func run() async throws -> Published {
        let signerN = try currentSigner()
        let created = try create()
        let prepared: Prepared
        do {
            let signerN1 = try currentSigner()
            try await register(signerN1, signerN)
            prepared = try prepare(created)
        } catch {
            try rollback()
            throw error
        }
        return try await publish(prepared, signerN)
    }
}

/// The retries a refused (`401`) signed request gets, in order. Pure, so the
/// order is pinned by a test rather than by reading the request loop.
public enum DsUnauthorizedRetry: Equatable {
    /// Re-sign with the previous epoch's signer.
    case previousEpoch
    /// Claim the journal on open, then re-sign with the signer that returns.
    case reclaim

    /// At most one of each, previous epoch first: it costs one request and no
    /// write, and it is the common case (a read racing a lagging registration).
    public static func plan(hasPrevious: Bool, hasReclaim: Bool) -> [DsUnauthorizedRetry] {
        var steps: [DsUnauthorizedRetry] = []
        if hasPrevious { steps.append(.previousEpoch) }
        if hasReclaim { steps.append(.reclaim) }
        return steps
    }
}

/// ADR-0053 open question 2 — the server answers a wrong clock with the same
/// `401` as any other refused proof, so the client decides the copy itself:
/// when the server's `Date` is more than a minute from this device's clock,
/// the refusal is almost certainly the clock.
public enum DsClockSkew {
    /// The tolerance past which the refusal is blamed on the clock.
    public static let toleranceSeconds: TimeInterval = 60

    /// The copy a final refusal shows when the clock is off.
    public static let copy = "Check this device's date and time."

    /// `true` when `serverDateHeader` parses and differs from `now` by more
    /// than ``toleranceSeconds``. An absent or unparseable header is not
    /// evidence of skew.
    public static func isSkewed(serverDateHeader: String?, now: Date) -> Bool {
        guard let header = serverDateHeader, let server = parseHttpDate(header) else {
            return false
        }
        return abs(server.timeIntervalSince(now)) > toleranceSeconds
    }

    /// RFC 9110 IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the only form a
    /// server may send.
    static func parseHttpDate(_ value: String) -> Date? {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "GMT")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        return formatter.date(from: value.trimmingCharacters(in: .whitespaces))
    }
}

/// Remembers, per journal, the newest signer this device derived and the one
/// before it, so a refused request can be retried under the previous epoch
/// (contract point 5) and a claim-on-open can be proven under an older key
/// (point 2).
///
/// Only ever holds signers this device derived itself; nothing here is
/// persisted, so after a relaunch there is no previous signer until the epoch
/// moves again, which is exactly "when the device still holds it".
@MainActor
public final class DsSignerMemory {
    private var current: [String: DsRequestSigner] = [:]
    private var previous: [String: DsRequestSigner] = [:]

    public init() {}

    /// Record `signer` as the journal's current signer and return the
    /// previous-epoch signer, if one is held for an OLDER epoch.
    @discardableResult
    public func observe(journal: String, signer: DsRequestSigner) -> DsRequestSigner? {
        if let held = current[journal] {
            if held.epoch() < signer.epoch() {
                previous[journal] = held
            } else if held.epoch() > signer.epoch() {
                // The group went back (a reset to the durable snapshot, an
                // abandoned sweep): a remembered "previous" newer than the
                // current epoch is not a previous epoch any more.
                if let prev = previous[journal], prev.epoch() >= signer.epoch() {
                    previous[journal] = nil
                }
            }
        }
        current[journal] = signer
        return previousSigner(journal: journal, before: signer.epoch())
    }

    /// The remembered signer for an epoch strictly below `epoch`, or `nil`.
    public func previousSigner(journal: String, before epoch: UInt64) -> DsRequestSigner? {
        guard let prev = previous[journal], prev.epoch() < epoch else { return nil }
        return prev
    }

    public func forget(journal: String) {
        current[journal] = nil
        previous[journal] = nil
    }

    public func reset() {
        current.removeAll()
        previous.removeAll()
    }
}

/// ADR-0053 client contract point 4 — heal on apply. When a batch moved the
/// journal's epoch from `before` to `after`, register `after`'s key proven
/// under `before`. Best effort: a refusal is expected and ignored (another
/// member registered first, or this device was too far behind). Returns
/// whether a registration was attempted and accepted.
@discardableResult
public func dsHealRegistration(
    ds: DsClient,
    groupId: Data,
    before: DsGroupSigner?,
    after: DsGroupSigner?
) async -> Bool {
    guard let before, let after, after.epoch() > before.epoch() else { return false }
    do {
        try await ds.claimGroup(
            groupId: groupId, bodyJson: after.groupClaimBodyJson(), provenBy: before)
        return true
    } catch {
        NSLog("ADR-0053: heal-on-apply registration not accepted (expected when another "
              + "member registered first): %@", String(describing: error))
        return false
    }
}

/// The link joiner's side of the knock (contract point 6): probe for the
/// epoch, build the proposal for it, knock with the attribution and the
/// proposal. No journal read. Each step injected, so the sequence is pinned by
/// a test with fakes.
public struct DsKnockJoin {
    /// A fresh PoW proof for one knock (challenge + solve).
    public var solvePow: () async throws -> PowProofPayload
    /// `POST /ds/group/<hex>/knock`; returns the epoch the response carries.
    public var knock: (PowProofPayload, [Data]) async throws -> UInt64?
    /// Build the external-join proposal for an epoch.
    public var createProposal: (UInt64) throws -> Data
    /// The join attribution, or `nil` when it could not be composed (it is
    /// advisory; the proposal goes regardless).
    public var attribution: () -> Data?

    public init(
        solvePow: @escaping () async throws -> PowProofPayload,
        knock: @escaping (PowProofPayload, [Data]) async throws -> UInt64?,
        createProposal: @escaping (UInt64) throws -> Data,
        attribution: @escaping () -> Data?
    ) {
        self.solvePow = solvePow
        self.knock = knock
        self.createProposal = createProposal
        self.attribution = attribution
    }

    /// Run the sequence. Returns the epoch the proposal targets.
    ///
    /// A probe answering with no epoch means the group was never claimed: no
    /// proposal for a guessed epoch is sent, because a wrong epoch is dropped
    /// by every member in silence and the joiner would wait forever.
    @discardableResult
    public func run() async throws -> UInt64 {
        let probePow = try await solvePow()
        guard let epoch = try await knock(probePow, []) else {
            throw DsKnockJoinError.groupNotClaimed
        }
        let proposal = try createProposal(epoch)
        var rows: [Data] = []
        if let attribution = attribution() { rows.append(attribution) }
        rows.append(proposal)
        let pow = try await solvePow()
        _ = try await knock(pow, rows)
        return epoch
    }
}

public enum DsKnockJoinError: Error, LocalizedError {
    /// The group's journal has no registered access key, so its epoch is not
    /// known to the server.
    case groupNotClaimed

    public var errorDescription: String? {
        switch self {
        case .groupNotClaimed:
            return "This group is not taking requests right now. Try again later."
        }
    }
}
