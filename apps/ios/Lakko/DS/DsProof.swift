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

/// A group-journal signer that also names the key it proves with — the epoch
/// key an ADR-0057 registration carries. The FFI `DsProofSigner` of a
/// conversation group, DeviceGroup or PublisherGroup.
public protocol DsGroupSigner: DsRequestSigner {
    /// The 32-byte Ed25519 verifying key registered for this epoch.
    func verifyingKey() -> Data
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
    /// ADR-0057 obligation 2 — the owner's lazy claim of an unclaimed journal,
    /// returning the signer to retry with; `nil` for a member (a member never
    /// claims) or when there is nothing to retry with.
    public var reclaim: (() async -> DsRequestSigner?)?
    /// ADR-0057 second security pass, finding 1 — told when the request is
    /// still refused (`401`) after every retry and the server's clock agrees
    /// with this one: the journal may have been pushed past this device's
    /// epoch, so an owner or admin schedules the repair check
    /// (``JournalRepair``). Only notes; never sends anything itself.
    public var onRefusedAtHead: (() -> Void)?

    public init(
        signer: DsRequestSigner,
        previous: DsRequestSigner? = nil,
        reclaim: (() async -> DsRequestSigner?)? = nil,
        onRefusedAtHead: (() -> Void)? = nil
    ) {
        self.signer = signer
        self.previous = previous
        self.reclaim = reclaim
        self.onRefusedAtHead = onRefusedAtHead
    }
}

/// ADR-0081 — what a failed commit request means.
public enum DsCommitFailure {
    /// `true` for a lost commit (`409`) and a definitive refusal (`400`,
    /// `401`, `403`): nothing of the commit is or will be on the DS.
    public static func isLost(_ error: Error) -> Bool {
        if (error as? DsStatementError) == .conflict { return true }
        if let chat = error as? ChatError, case .wrapped(_, let underlying) = chat {
            return isLost(underlying)
        }
        return SweepRegistrationBackoff.appliesTo(error)
    }
}

/// Whether a DS request failed before it reached the DS (the caller cannot
/// tell whether it took effect).
public enum DsTransientFailure {
    public static func isNetwork(_ error: Error) -> Bool {
        if let ds = error as? DsClientError, case .network = ds { return true }
        if let chat = error as? ChatError, case .wrapped(_, let underlying) = chat {
            return isNetwork(underlying)
        }
        return false
    }
}

/// The retries a refused (`401`) signed request gets, in order. Pure, so the
/// order is pinned by a test rather than by reading the request loop.
public enum DsUnauthorizedRetry: Equatable {
    /// Re-sign with the previous epoch's signer.
    case previousEpoch
    /// The owner's lazy claim (ADR-0057), then re-sign with the signer that
    /// returns.
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
    public static var copy: String { Localization.text("ds.clock_skew") }

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

// ADR-0057 — heal-on-apply (ADR-0053 point 4) is gone: a plain member no
// longer registers an epoch key it did not commit. On a `401` it waits for the
// committer's registration and retries on its next poll.

/// The link joiner's side of the knock (contract point 6): probe for the
/// epoch, build the proposal for it, knock with the attribution and the
/// proposal. No journal read. Each step injected, so the sequence is pinned by
/// a test with fakes.
///
/// Each knock (the probe included) spends one `ds-knock` anonymous token
/// (ADR-0054 S3); ``DsClient/knock(groupId:payloads:)`` retries once with a
/// second token on a token refusal, so nothing is retried here.
public struct DsKnockJoin {
    /// `POST /ds/group/<hex>/knock`; returns the epoch the response carries.
    public var knock: ([Data]) async throws -> UInt64?
    /// Build the external-join proposal for an epoch.
    public var createProposal: (UInt64) throws -> Data
    /// The join attribution, or `nil` when it could not be composed (it is
    /// advisory; the proposal goes regardless).
    public var attribution: () -> Data?
    /// Make the joiner's state durable once the proposal is built and before
    /// it is published. The proposal carries a fresh KeyPackage whose private
    /// keys are the only thing that opens the Welcome an admin sends later,
    /// and they live in the joiner's in-memory provider: a proposal published
    /// before they are on disk can be approved into a Welcome nobody can
    /// open. Throwing stops the sequence before the proposal goes out.
    public var persistBeforeKnock: () async throws -> Void

    public init(
        knock: @escaping ([Data]) async throws -> UInt64?,
        createProposal: @escaping (UInt64) throws -> Data,
        attribution: @escaping () -> Data?,
        persistBeforeKnock: @escaping () async throws -> Void = {}
    ) {
        self.knock = knock
        self.createProposal = createProposal
        self.attribution = attribution
        self.persistBeforeKnock = persistBeforeKnock
    }

    /// Run the sequence. Returns the epoch the proposal targets.
    ///
    /// A probe answering with no epoch means the group was never claimed: no
    /// proposal for a guessed epoch is sent, because a wrong epoch is dropped
    /// by every member in silence and the joiner would wait forever.
    @discardableResult
    public func run() async throws -> UInt64 {
        guard let epoch = try await knock([]) else {
            throw DsKnockJoinError.groupNotClaimed
        }
        let proposal = try createProposal(epoch)
        // A refused or failed persist publishes nothing.
        try await persistBeforeKnock()
        var rows: [Data] = []
        if let attribution = attribution() { rows.append(attribution) }
        rows.append(proposal)
        _ = try await knock(rows)
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
            return JoinRequestFailure.notTakingRequests.message
        }
    }
}

/// ADR-0053 point 7 — at most one knock read per group per ``floorSeconds``.
///
/// Knocks are polled next to the group journal, and the journal is polled far
/// more often than a join request arrives (measured live: about twice a second
/// across the groups one owner holds). ``floorSeconds`` (10 s) is the floor
/// of an event-driven read (the members sheet's join attributions);
/// ``backgroundFloorSeconds`` (60 s, 2026-11-06) is the floor of the poll an
/// owner / admin device runs beside its journal reads, so an idle client does
/// not read knocks every few seconds. Both chosen, not measured; web and
/// Android use the same two.
@MainActor
public final class KnockPollThrottle {
    public nonisolated static let floorSeconds: TimeInterval = 10
    public nonisolated static let backgroundFloorSeconds: TimeInterval = 60

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
/// catalog copy all three clients share (ADR-0061), rendered in the language
/// of the moment ``message`` is read. Never a raw error: the error is logged.
public enum JoinRequestFailure: Equatable {
    /// The group's journal has no registered epoch (the probe returned none).
    case notTakingRequests
    /// A signed request was refused and the device clock is off.
    case clockSkew
    /// The discovery ledger answered `429` on the invite fetch or consume:
    /// the link may be fine, the server asked this network to slow down.
    case tryLater
    /// ADR-0054 S3 — today's allowance of link consumes / knocks is used up
    /// (`anon-token-quota-exhausted`); nothing to retry until tomorrow.
    case allowanceUsed
    /// Anything else.
    case generic

    public var message: String {
        switch self {
        case .notTakingRequests:
            return Localization.text("group.join.not_taking_requests")
        case .clockSkew:
            return DsClockSkew.copy
        case .tryLater:
            return inviteRateLimitedMessage
        case .allowanceUsed:
            return AnonTokenCopy.allowanceUsed
        case .generic:
            return Localization.text("group.join.request_failed")
        }
    }

    /// The generic failure and a rate-limit refusal offer "Try again" (the
    /// latter once the minute has passed); the other two are not fixed by
    /// pressing a button.
    public var offersRetry: Bool { self == .generic || self == .tryLater }

    public static func classify(_ error: Error) -> JoinRequestFailure {
        if let knock = error as? DsKnockJoinError, knock == .groupNotClaimed {
            return .notTakingRequests
        }
        if DsClientError.clockSkewCopy(in: error) != nil { return .clockSkew }
        if DiscoveryClientError.isRateLimited(error) { return .tryLater }
        if AnonTokenUnavailable.isQuotaExhausted(error) { return .allowanceUsed }
        return .generic
    }
}

/// The join screen's success copy and its controls, in the interface
/// language of the moment they are read.
public var joinSucceededMessage: String { Localization.text("group.join.succeeded") }
public var joinOpenChat: String { Localization.text("group.join.open_chat") }
public var joinTryAgain: String { Localization.text("common.try_again") }
