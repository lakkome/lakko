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
///
/// A knock POST refused with `401` means its proof of work expired while it
/// was being solved (a challenge lives 300 s and a phone may take minutes at
/// a higher effort): the knock is retried ONCE with a freshly fetched and
/// solved challenge before the failure surfaces.
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
    /// Whether a knock failure is an expired proof of work.
    public var isPowExpired: (Error) -> Bool

    public init(
        solvePow: @escaping () async throws -> PowProofPayload,
        knock: @escaping (PowProofPayload, [Data]) async throws -> UInt64?,
        createProposal: @escaping (UInt64) throws -> Data,
        attribution: @escaping () -> Data?,
        isPowExpired: @escaping (Error) -> Bool = DsKnockJoin.isUnauthorized
    ) {
        self.solvePow = solvePow
        self.knock = knock
        self.createProposal = createProposal
        self.attribution = attribution
        self.isPowExpired = isPowExpired
    }

    /// A knock carries no proof header, so the only `401` it can get is the
    /// PoW verifier's (an expired or already-used challenge).
    public static func isUnauthorized(_ error: Error) -> Bool {
        if let ds = error as? DsClientError, case .http(let status, _, _) = ds {
            return status == 401
        }
        return false
    }

    /// Run the sequence. Returns the epoch the proposal targets.
    ///
    /// A probe answering with no epoch means the group was never claimed: no
    /// proposal for a guessed epoch is sent, because a wrong epoch is dropped
    /// by every member in silence and the joiner would wait forever.
    @discardableResult
    public func run() async throws -> UInt64 {
        guard let epoch = try await knockPaying([]) else {
            throw DsKnockJoinError.groupNotClaimed
        }
        let proposal = try createProposal(epoch)
        var rows: [Data] = []
        if let attribution = attribution() { rows.append(attribution) }
        rows.append(proposal)
        _ = try await knockPaying(rows)
        return epoch
    }

    /// One knock with its own proof of work, retried once on an expired one.
    private func knockPaying(_ payloads: [Data]) async throws -> UInt64? {
        let pow = try await solvePow()
        do {
            return try await knock(pow, payloads)
        } catch let error where isPowExpired(error) {
            let fresh = try await solvePow()
            return try await knock(fresh, payloads)
        }
    }
}

public enum DsKnockJoinError: Error, LocalizedError {
    /// The group's journal has no registered access key, so its epoch is not
    /// known to the server.
    case groupNotClaimed

    public var errorDescription: String? {
        switch self {
        case .groupNotClaimed:
            return JoinRequestFailure.notTakingRequests.message
        }
    }
}

/// ADR-0053 point 7 — at most one knock read per group per ``floorSeconds``.
///
/// Knocks are polled next to the group journal, and the journal is polled far
/// more often than a join request arrives (measured live: about twice a second
/// across the groups one owner holds). The floor matches Android's.
@MainActor
public final class KnockPollThrottle {
    public nonisolated static let floorSeconds: TimeInterval = 10

    private let floor: TimeInterval
    private var lastAt: [String: Date] = [:]

    public init(floorSeconds: TimeInterval = KnockPollThrottle.floorSeconds) {
        floor = floorSeconds
    }

    /// `true` when `key` may be polled at `now`, and records the poll. A clock
    /// that went backwards counts as due, so a skewed clock cannot silence
    /// the knocks for good.
    public func take(_ key: String, now: Date = Date()) -> Bool {
        if let last = lastAt[key], now >= last, now.timeIntervalSince(last) < floor {
            return false
        }
        lastAt[key] = now
        return true
    }

    public func reset() {
        lastAt.removeAll()
    }
}

/// ADR-0053 — whether the members sheet re-reads the join attributions.
///
/// The attribution read goes over the whole knock queue, and the sheet asks
/// on every poll result while it is open (measured live: 45 reads in 60 s).
/// It needs to ask only when the set of pending joiners differs from the set
/// the shown map was read for, and even then at most once per floor.
public enum JoinAttributionRefresh {
    public enum Decision: Equatable {
        /// Nothing changed (or a read is already running, or the floor is not
        /// yet due): keep the map on screen.
        case keep
        /// No pending joiner: empty the map, read nothing.
        case clear
        /// Read the attributions for the current pending set.
        case fetch
    }

    /// `allowRead` is asked only when a read would otherwise happen, so the
    /// floor is not spent on a refresh that needs nothing.
    public static func decide(
        pending: Set<Data>,
        fetchedFor: Set<Data>?,
        inFlight: Bool,
        allowRead: () -> Bool
    ) -> Decision {
        if pending.isEmpty { return fetchedFor == [] ? .keep : .clear }
        if pending == fetchedFor || inFlight { return .keep }
        return allowRead() ? .fetch : .keep
    }
}

/// The join screen's failure states (`docs/ux/group-invite-page.md`), in the
/// exact copy all three clients share. Never a raw error: the error is logged.
public enum JoinRequestFailure: Equatable {
    /// The group's journal has no registered epoch (the probe returned none).
    case notTakingRequests
    /// A signed request was refused and the device clock is off.
    case clockSkew
    /// Anything else.
    case generic

    public var message: String {
        switch self {
        case .notTakingRequests:
            return "This group isn't taking requests yet. Try again later."
        case .clockSkew:
            return DsClockSkew.copy
        case .generic:
            return "Your request didn't go through. Check your connection and try again."
        }
    }

    /// Only the generic failure offers "Try again": the other two are not
    /// fixed by pressing a button now.
    public var offersRetry: Bool { self == .generic }

    public static func classify(_ error: Error) -> JoinRequestFailure {
        if let knock = error as? DsKnockJoinError, knock == .groupNotClaimed {
            return .notTakingRequests
        }
        if DsClientError.clockSkewCopy(in: error) != nil { return .clockSkew }
        return .generic
    }
}

/// The join screen's success copy and its controls.
public let joinSucceededMessage = "You joined the group."
public let joinOpenChat = "Open chat"
public let joinTryAgain = "Try again"
