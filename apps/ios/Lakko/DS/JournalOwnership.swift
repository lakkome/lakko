// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import LakkoCore

// ADR-0057 — a journal has an owner; only those who change a group register
// its epochs.
//
// Every group journal on the Delivery Service has a ROOT key, derived on every
// device of the owner's account from the account identity and the group id
// (`dsJournalRootSigner`). Epoch keys are registered only by the root or by a
// REGISTRAR the root appointed, in two ranks (admin, member), and the root
// hands each registrar its seed inside the group, sealed to that member's
// account X25519 key (`composeRegistrarSeedEnvelope`).
//
// This file holds what every caller shares: the statement transport, which
// signer a device registers with, the pure appointment / receive / revoke
// rules, and the per-account store of this device's registrar seeds. The
// statements themselves (canonical bytes, signature, route, JSON) are the Rust
// core's; nothing here spells one.

// MARK: - Transport

/// Anything that can send one ADR-0057 ownership statement. The production
/// value is ``DsClient``; a protocol so a test can record the exact requests.
public protocol DsStatementSending: AnyObject {
    /// `PUT <request.path>` with `request.bodyJson`, no proof header. Throws
    /// ``DsStatementError`` on any non-2xx answer.
    func putStatement(_ request: DsStatementRequest) async throws
}

/// ADR-0081 decision 1 — what the commit route answered: the journal row's
/// id and time, and whether it was the identical re-send of a commit that had
/// already landed (`200`, the original row) rather than a new one (`201`).
public struct DsCommitAnswer: Equatable {
    public let id: Int64
    public let createdAt: String
    public let resent: Bool

    public init(id: Int64, createdAt: String, resent: Bool) {
        self.id = id
        self.createdAt = createdAt
        self.resent = resent
    }
}

/// Anything that can send one ADR-0081 commit. The production value is
/// ``DsClient``; a protocol so a test can model the DS.
public protocol DsCommitSending: AnyObject {
    /// `POST <request.path>` with `request.bodyJson` and the request proof of
    /// `proof` over those exact bytes. Throws ``DsStatementError`` on a
    /// refusal (`409` is ``DsStatementError/conflict``) and
    /// ``DsClientError/network(endpoint:underlying:)`` when the DS was not
    /// reached (the caller cannot tell whether the commit landed; the
    /// identical re-send says).
    func postCommit(_ request: DsStatementRequest, proof: DsRequestSigner) async throws -> DsCommitAnswer
}

/// A refused ownership statement, by the status the DS answered.
public enum DsStatementError: Error, Equatable, LocalizedError {
    /// `400` — malformed, the wrong journal, or an epoch above `2^48 - 1`.
    case malformed(String)
    /// `401 proof required` — a bad signature, a stranger signer, or a
    /// non-claim route on an unclaimed journal.
    case unauthorized
    /// `403` — the signer's rank is too low for what it asked.
    case rankShortfall
    /// `409` — a conflict (a different claim, a lower epoch), after the one
    /// retry a lock race gets.
    case conflict
    /// `503 key-reset-disabled` — the DS has no pinned attest key, so an
    /// attested reset (ADR-0073) cannot be verified there.
    case keyResetDisabled
    /// Any other status.
    case http(status: Int, body: String)

    /// Map a DS answer to the refusal it is.
    public static func from(status: Int, body: String) -> DsStatementError {
        switch status {
        case 400: return .malformed(body)
        case 401: return .unauthorized
        case 403: return .rankShortfall
        case 409: return .conflict
        case 503 where body.contains("key-reset-disabled"): return .keyResetDisabled
        default: return .http(status: status, body: body)
        }
    }

    public var errorDescription: String? {
        switch self {
        case .malformed(let body): return "ds statement refused (400): \(body)"
        case .unauthorized: return "ds statement refused (401 proof required)"
        case .rankShortfall: return "ds statement refused (403 rank)"
        case .conflict: return "ds statement refused (409 conflict)"
        case .keyResetDisabled: return "ds key reset disabled (503)"
        case .http(let status, let body): return "ds statement refused (\(status)): \(body)"
        }
    }
}

// MARK: - Who registers

/// The key this device registers a journal's epochs with.
public enum JournalAuthority {
    /// This account owns the group: the root key, which outranks every
    /// registrar and may revoke any of them.
    case root(DsStatementSignerProtocol)
    /// The owner appointed this account: its registrar key at `rank`.
    case registrar(DsStatementSignerProtocol, DsRegistrarRank)

    public var signer: DsStatementSignerProtocol {
        switch self {
        case .root(let signer), .registrar(let signer, _): return signer
        }
    }

    public var isRoot: Bool {
        if case .root = self { return true }
        return false
    }

    /// The root or an admin registrar: the two ranks that repair a journal a
    /// lower rank pushed ahead (second security pass, finding 1) and bind the
    /// push routing id (finding 5). A member does neither.
    public var isRootOrAdmin: Bool {
        switch self {
        case .root: return true
        case .registrar(_, let rank): return rank == .admin
        }
    }

    /// Resolve the authority: the root when this account owns the group, else
    /// the registrar seed the owner sent (when this device holds one), else
    /// `nil` — a device with neither cannot register an epoch and so must not
    /// commit (obligation 5): it leaves the sweep to an admin or the owner.
    ///
    /// Second security pass, finding 4: owning the group in its role table and
    /// holding its journal root on the DS are two facts while an ownership
    /// transfer is in flight. An owner whose "root held" marker is not set
    /// (the takeover has not landed) keeps signing with the registrar seed the
    /// old owner appointed it with; with no such seed it falls back to the
    /// root (a group founded before the marker existed; the probe sets it).
    ///
    /// The root is derived only for the owner, never tried speculatively: a
    /// non-owner's "root" is a key the DS has never heard of.
    public static func resolve(
        isOwner: Bool,
        rootHeld: Bool = true,
        rootSigner: () throws -> DsStatementSignerProtocol,
        registrar: () -> (DsStatementSignerProtocol, DsRegistrarRank)?
    ) -> JournalAuthority? {
        if isOwner {
            if !rootHeld, let (signer, rank) = registrar() { return .registrar(signer, rank) }
            if let root = try? rootSigner() { return .root(root) }
        }
        if let (signer, rank) = registrar() { return .registrar(signer, rank) }
        return nil
    }
}

// MARK: - Pure rules

public enum JournalOwnership {

    /// The ranks the owner wants on the journal, per member ACCOUNT key: admin
    /// for the ADR-0022 admins, member for everyone else. The owner's own
    /// account (every one of its devices holds the root) is not a registrar.
    public static func desiredRanks(
        memberAccountKeys: [Data],
        owner: Data,
        admins: [Data]
    ) -> [Data: DsRegistrarRank] {
        let adminSet = Set(admins)
        var out: [Data: DsRegistrarRank] = [:]
        for key in memberAccountKeys where key != owner && key.count == 32 {
            out[key] = adminSet.contains(key) ? .admin : .member
        }
        return out
    }

    // MARK: Converge the registrar set (second security pass, finding 2)

    /// The most entries one `registrars-set` statement may carry (the core's
    /// `MAX_REGISTRARS`).
    public static let maxRegistrarSet = 4096

    /// A deterministic fingerprint of a desired set (sorted `key:rank`
    /// pairs), so the owner sends `registrars-set` only when the set it wants
    /// differs from the one it last sent this session.
    public static func registrarSetFingerprint(_ desired: [Data: DsRegistrarRank]) -> String {
        desired
            .sorted { $0.key.lexicographicallyPrecedes($1.key) }
            .map { "\($0.key.base64EncodedString()):\($0.value == .admin ? "admin" : "member")" }
            .joined(separator: ",")
    }

    /// The whole `registrars-set` for `desired`: one entry per member ACCOUNT,
    /// carrying the registrar PUBLIC key the root derives for it (never the
    /// account key), sorted by account key. `nil` when a key cannot be derived
    /// or the set exceeds the server's bound: nothing is sent then.
    public static func registrarSetEntries(
        desired: [Data: DsRegistrarRank],
        registrarPublicKey: (Data) -> Data?
    ) -> [DsRegistrarEntry]? {
        guard desired.count <= maxRegistrarSet else { return nil }
        var out: [DsRegistrarEntry] = []
        for (account, rank) in desired.sorted(by: { $0.key.lexicographicallyPrecedes($1.key) }) {
            guard let key = registrarPublicKey(account) else { return nil }
            out.append(DsRegistrarEntry(publicKey: key, rank: rank))
        }
        return out
    }

    /// The keys a delivered seed envelope is recorded under for `account`
    /// (ADR-0057, amended 2026-10-01: per DEVICE): the leaf signature keys the
    /// roster holds for that account, or the account key itself when none is
    /// known. One envelope (sealed to the ACCOUNT key) serves every device of
    /// the account; recording it per device is what lets a device that joined
    /// after the envelope was posted get its own copy at the next reconcile.
    public static func deliveryKeys(account: Data, leaves: [Data: [Data]]) -> [Data] {
        let known = leaves[account] ?? []
        return known.isEmpty ? [account] : known
    }

    /// The accounts whose seed envelope is owed: every desired account one of
    /// whose devices (``deliveryKeys(account:leaves:)``) has no delivered
    /// record at the desired rank. Sorted by account key.
    public static func envelopesOwed(
        desired: [Data: DsRegistrarRank],
        leaves: [Data: [Data]] = [:],
        delivered: [Data: DsRegistrarRank]
    ) -> [(accountKey: Data, rank: DsRegistrarRank)] {
        desired
            .filter { account, rank in
                deliveryKeys(account: account, leaves: leaves).contains { delivered[$0] != rank }
            }
            .map { (accountKey: $0.key, rank: $0.value) }
            .sorted { $0.accountKey.lexicographicallyPrecedes($1.accountKey) }
    }

    /// What one reconcile pass needs. Everything is a closure so a host test
    /// drives the rule with fakes.
    public struct RegistrarReconcilePorts {
        /// The desired set now (from the live roster and role table), or `nil`
        /// when this account is not the owner or the roster is unreadable.
        public var desired: () -> [Data: DsRegistrarRank]?
        /// This device's authority now; only the root reconciles.
        public var authority: () -> JournalAuthority?
        /// The registrar public key the root derives for a member account.
        public var registrarPublicKey: (Data) -> Data?
        /// The fingerprint this session last sent for the group, if any.
        public var lastSentSet: () -> String?
        public var noteSentSet: (String) -> Void
        /// Each desired account's leaf signature keys in the live roster: a
        /// delivery is recorded per device (``deliveryKeys(account:leaves:)``).
        public var memberLeaves: () -> [Data: [Data]]
        /// Which devices (leaf keys; an account key where no leaf was known)
        /// hold their account's seed envelope, at which rank.
        public var loadDelivered: () -> [Data: DsRegistrarRank]
        public var saveDelivered: ([Data: DsRegistrarRank]) -> Void
        /// Seal and publish one member's seed envelope; `false` when its
        /// X25519 key does not resolve yet (left pending), throws on a failed
        /// send (also left pending).
        public var sendEnvelope: (_ accountKey: Data, _ rank: DsRegistrarRank) async throws -> Bool
        public var journalId: Data
        public var ds: DsStatementSending

        public init(
            journalId: Data,
            ds: DsStatementSending,
            desired: @escaping () -> [Data: DsRegistrarRank]?,
            authority: @escaping () -> JournalAuthority?,
            registrarPublicKey: @escaping (Data) -> Data?,
            lastSentSet: @escaping () -> String?,
            noteSentSet: @escaping (String) -> Void,
            memberLeaves: @escaping () -> [Data: [Data]] = { [:] },
            loadDelivered: @escaping () -> [Data: DsRegistrarRank],
            saveDelivered: @escaping ([Data: DsRegistrarRank]) -> Void,
            sendEnvelope: @escaping (Data, DsRegistrarRank) async throws -> Bool
        ) {
            self.journalId = journalId
            self.memberLeaves = memberLeaves
            self.ds = ds
            self.desired = desired
            self.authority = authority
            self.registrarPublicKey = registrarPublicKey
            self.lastSentSet = lastSentSet
            self.noteSentSet = noteSentSet
            self.loadDelivered = loadDelivered
            self.saveDelivered = saveDelivered
            self.sendEnvelope = sendEnvelope
        }
    }

    /// What one reconcile pass did.
    public enum RegistrarReconcileOutcome: Equatable {
        /// Not the owner, not holding the root, or the roster is unreadable.
        case notOwner
        /// Nothing to send.
        case unchanged
        /// `setSent` when a `registrars-set` went out; `pending` accounts whose
        /// envelope is left for the next pass.
        case reconciled(setSent: Bool, pending: Int)
    }

    /// One reconcile pass (finding 2): `registrars-set` with the WHOLE desired
    /// set when it differs from what this session last sent (the DS drops
    /// every registrar not listed, so an evicted member's key goes whatever a
    /// device-local record says), then one seed envelope per account whose
    /// delivered rank differs. A refused statement throws (nothing recorded,
    /// the next pass retries); a failed envelope leaves only that member
    /// pending. The delivered record never decides what the DS holds.
    public static func reconcileRegistrars(
        _ ports: RegistrarReconcilePorts
    ) async throws -> RegistrarReconcileOutcome {
        guard let desired = ports.desired() else { return .notOwner }
        guard let authority = ports.authority(), authority.isRoot else { return .notOwner }
        let fingerprint = registrarSetFingerprint(desired)
        let setDue = ports.lastSentSet() != fingerprint
        let delivered = ports.loadDelivered()
        let leaves = ports.memberLeaves()
        let owed = envelopesOwed(desired: desired, leaves: leaves, delivered: delivered)
        // The delivery record the roster still calls for: every device of every
        // desired account, at that account's rank. Anything else (a departed
        // device or account, an old rank) is dropped from the record.
        var live: [Data: DsRegistrarRank] = [:]
        for (account, rank) in desired {
            for key in deliveryKeys(account: account, leaves: leaves) { live[key] = rank }
        }
        let departed = delivered.contains { live[$0.key] != $0.value }
        if !setDue && owed.isEmpty && !departed { return .unchanged }
        if setDue {
            guard let entries = registrarSetEntries(
                desired: desired, registrarPublicKey: ports.registrarPublicKey) else {
                throw DsStatementError.malformed("registrar set not derivable")
            }
            try await ports.ds.putStatement(
                try authority.signer.registrarsSet(journalId: ports.journalId, entries: entries))
            ports.noteSentSet(fingerprint)
            NSLog("ADR-0057: registrar set sent (%d entries)", entries.count)
        }
        var next = delivered.filter { live[$0.key] == $0.value }
        var pending = 0
        for entry in owed {
            do {
                if try await ports.sendEnvelope(entry.accountKey, entry.rank) {
                    for key in deliveryKeys(account: entry.accountKey, leaves: leaves) {
                        next[key] = entry.rank
                    }
                } else {
                    pending += 1
                }
            } catch {
                NSLog("ADR-0057: registrar seed envelope not sent; retried later")
                pending += 1
            }
        }
        ports.saveDelivered(next)
        return .reconciled(setSent: setDue, pending: pending)
    }

    /// Why a received registrar-seed envelope was kept or refused.
    public enum SeedVerdict: Equatable {
        case accept
        /// Addressed to another account: not ours to open.
        case notAddressedToUs
        /// The MLS sender does not resolve, at receipt, to the group's owner
        /// through the live role table. Only the root appoints.
        case senderNotOwner
        /// The envelope names another journal than the group it arrived in.
        case wrongJournal
    }

    /// The receive gate (obligation 4). `senderAccountKey` is the sender leaf
    /// resolved to its ACCOUNT key at receipt (C-68); `owner` the role table's
    /// owner in the same read. A group with no role table has no owner, so it
    /// appoints nobody.
    public static func seedVerdict(
        envelopeJournal: Data,
        envelopeAddressee: Data,
        groupId: Data,
        ownAccountKey: Data,
        senderAccountKey: Data?,
        owner: Data?
    ) -> SeedVerdict {
        guard envelopeAddressee == ownAccountKey else { return .notAddressedToUs }
        guard envelopeJournal == groupId else { return .wrongJournal }
        guard let owner, let senderAccountKey, senderAccountKey == owner else {
            return .senderNotOwner
        }
        return .accept
    }

    /// The accounts a commit EVICTS: those every one of whose leaves the
    /// commit removes (ADR-0050 — removing one device of an account leaves the
    /// account a member). `members` is the roster BEFORE the commit.
    public static func evictedAccountKeys(
        members: [(leaf: UInt32, accountKey: Data)],
        removedLeaves: Set<UInt32>
    ) -> [Data] {
        guard !removedLeaves.isEmpty else { return [] }
        var leavesByAccount: [Data: [UInt32]] = [:]
        for member in members { leavesByAccount[member.accountKey, default: []].append(member.leaf) }
        return leavesByAccount
            .filter { _, leaves in leaves.allSatisfy { removedLeaves.contains($0) } }
            .map(\.key)
            .sorted { $0.lexicographicallyPrecedes($1) }
    }

    /// The revoke list of a registration (ADR-0057, amended 2026-10-01:
    /// whoever removes a person revokes every registrar key of that person in
    /// the same registration).
    ///
    /// - The root derives the evicted accounts' registrar keys itself.
    /// - A registrar (an admin) cannot derive them; it revokes the keys it
    ///   learned from the registrar-seed envelopes the owner posted
    ///   (`knownRegistrar`, account -> registrar public key and rank). Only
    ///   keys of a STRICTLY lower rank than the signer's are listed: the DS
    ///   refuses the whole registration (`403`) when one revocation names an
    ///   equal or higher rank, so an admin never lists another admin (removing
    ///   an admin stays the root's, as before, and the owner's next
    ///   `registrars-set` remains the backstop). `equalOrHigherAccounts` are
    ///   the accounts the LIVE role table ranks at or above the signer (the
    ///   owner and, for an admin, the admins): skipped even when the recorded
    ///   rank is older and lower, since a member promoted since its envelope
    ///   keeps the same registrar key at the higher rank.
    /// - A member registrar revokes nothing (nobody ranks below it).
    public static func revokeList(
        authority: JournalAuthority,
        evictedAccountKeys: [Data],
        registrarPublicKey: (Data) -> Data?,
        knownRegistrar: (Data) -> RegistrarSeedStore.KnownRegistrar? = { _ in nil },
        equalOrHigherAccounts: Set<Data> = []
    ) -> [Data] {
        switch authority {
        case .root:
            return evictedAccountKeys.compactMap(registrarPublicKey)
        case .registrar(_, let signerRank):
            return evictedAccountKeys.compactMap { account in
                guard !equalOrHigherAccounts.contains(account),
                      let known = knownRegistrar(account),
                      rankOrder(known.rank) < rankOrder(signerRank) else { return nil }
                return known.publicKey
            }
        }
    }

    /// The DS order of the registrar ranks (member below admin).
    static func rankOrder(_ rank: DsRegistrarRank) -> Int {
        switch rank {
        case .member: return 1
        case .admin: return 2
        }
    }

    /// ADR-0081 decision 1 — register `epochKey` at `epoch` with the device's
    /// authority, revoking `revoke` in the same statement, AND append `row`
    /// (the commit that reaches `epoch`) in ONE request: `POST .../commit`,
    /// proven by `proof`, the key of epoch `epoch - 1`. The DS accepts it only
    /// at `latest + 1`; the identical re-send (same key, same row) answers the
    /// original message id (`resent`), any other commit at a registered epoch
    /// is `409`. Since ADR-0081 (open question 1, decided 2026-10-18) this is
    /// the only way an epoch is appended: `PUT .../access` refuses an Append.
    ///
    /// A `409` is retried once with a freshly signed statement (a lost race
    /// for the group's lock row rolls back without deciding); a second `409`
    /// throws ``DsStatementError/conflict`` and `onConflict` is told the epoch
    /// this device is at, so the owner or an admin can schedule the repair
    /// check (``JournalRepair``). A commit path never replaces.
    @discardableResult
    public static func commitEpoch(
        authority: JournalAuthority,
        journalId: Data,
        epoch: UInt64,
        epochKey: Data,
        revoke: [Data],
        row: Data,
        proof: DsRequestSigner,
        ds: DsCommitSending,
        onConflict: ((_ currentEpoch: UInt64) -> Void)? = nil
    ) async throws -> DsCommitAnswer {
        guard !row.isEmpty else { throw DsStatementError.malformed("a commit row must be non-empty") }
        func once() async throws -> DsCommitAnswer {
            let request = try authority.signer.commit(
                journalId: journalId, epoch: epoch, epochKey: epochKey, revoke: revoke, row: row)
            return try await ds.postCommit(request, proof: proof)
        }
        do {
            return try await once()
        } catch DsStatementError.conflict {
            // Retried once below.
        }
        do {
            return try await once()
        } catch DsStatementError.conflict {
            NSLog("ADR-0081: the commit's epoch belongs to another commit (409); abandoned")
            if epoch > 0 { onConflict?(epoch - 1) }
            throw DsStatementError.conflict
        }
    }

    /// The repair (finding 1): REPLACE at `epoch` with the key registered for
    /// it, dropping every epoch above it that a strictly lower rank
    /// registered. Never called from a commit path. Throws the refusal (`403`:
    /// an equal or higher rank registered above).
    public static func replaceEpoch(
        authority: JournalAuthority,
        journalId: Data,
        epoch: UInt64,
        epochKey: Data,
        ds: DsStatementSending
    ) async throws {
        try await ds.putStatement(
            try authority.signer.replaceEpoch(
                journalId: journalId, epoch: epoch, epochKey: epochKey, revoke: []))
    }

    /// The answer of the "am I still the root?" probe (finding 4).
    public enum RootProbe: Equatable {
        /// `204`: the DS knows `root` as this journal's root.
        case root
        /// `401`: it does not (any more).
        case notRoot
    }

    /// The probe: a `root` statement signed by `root` naming itself. `204` for
    /// the current root, the uniform `401` for any other key. Any other
    /// refusal or a network failure throws.
    public static func probeRoot(
        root: DsStatementSignerProtocol,
        journalId: Data,
        ds: DsStatementSending
    ) async throws -> RootProbe {
        do {
            try await ds.putStatement(
                try root.rootTransfer(journalId: journalId, newRoot: root.publicKey()))
            return .root
        } catch DsStatementError.unauthorized {
            return .notRoot
        }
    }

    /// The owner's first claim of a journal it just founded (obligation 1).
    public static func claim(
        root: DsStatementSignerProtocol,
        journalId: Data,
        epoch: UInt64,
        epochKey: Data,
        ds: DsStatementSending
    ) async throws {
        try await ds.putStatement(
            try root.claim(journalId: journalId, epoch: epoch, epochKey: epochKey))
    }

    /// What a lazy claim did.
    public enum LazyClaimOutcome: Equatable {
        /// Not the owner: a member never claims.
        case notOwner
        /// The journal was unclaimed and is now claimed at this epoch.
        case claimed
        /// The journal is claimed already (`409`): nothing more was sent. This
        /// device waits for the committer's registration like any member.
        case alreadyClaimed
        /// The DS refused the claim.
        case failed
    }

    /// Obligation 2 — a device of the OWNER opens a group whose journal
    /// answered `401` to a signed read: claim it with the root at the current
    /// epoch (the claim registers that epoch). A non-owner does nothing.
    ///
    /// A `409` (claimed already, by another device of the owner or with a
    /// chain that moved on) is left alone: nothing is registered. A journal a
    /// lower rank pushed ahead is repaired from above by ``JournalRepair``,
    /// never from here. Web and Android do the same.
    public static func lazyClaim(
        isOwner: Bool,
        root: () throws -> DsStatementSignerProtocol,
        journalId: Data,
        epoch: UInt64,
        epochKey: Data,
        ds: DsStatementSending
    ) async -> LazyClaimOutcome {
        guard isOwner, let root = try? root() else { return .notOwner }
        do {
            try await claim(root: root, journalId: journalId, epoch: epoch, epochKey: epochKey, ds: ds)
            return .claimed
        } catch DsStatementError.conflict {
            return .alreadyClaimed
        } catch {
            NSLog("ADR-0057: lazy claim refused: %@", String(describing: error))
            return .failed
        }
    }

    /// Obligation 7 — the claim of a journal this device just (re-)founded
    /// under a deterministic id (a DeviceGroup, a PublisherGroup): claim; a
    /// `409` means the same root claimed the id before (the old group, lost
    /// with the local state), so the chain is replaced with a `reset`.
    ///
    /// Only for a group this device FOUNDED here. On an existing group a `409`
    /// usually means this device is behind, and a reset would wipe the epochs
    /// the others registered since.
    public static func claimOrReset(
        root: DsStatementSignerProtocol,
        journalId: Data,
        epoch: UInt64,
        epochKey: Data,
        ds: DsStatementSending
    ) async throws {
        do {
            try await claim(root: root, journalId: journalId, epoch: epoch, epochKey: epochKey, ds: ds)
        } catch DsStatementError.conflict {
            try await ds.putStatement(
                try root.reset(journalId: journalId, epoch: epoch, epochKey: epochKey, registrars: nil))
        }
    }

}

// MARK: - Founding marks

/// Journals this device founded under a deterministic id (a DeviceGroup, a
/// PublisherGroup) and has not yet claimed. The mark is what lets the claim
/// fall back to a root ``JournalOwnership/claimOrReset(root:journalId:epoch:epochKey:ds:)``
/// on the founding device only; a `409` anywhere else is never answered with a
/// reset. Plain `UserDefaults`: a journal id is a public DS address.
public final class JournalFoundingMarks {
    public static let shared = JournalFoundingMarks()

    private let defaults: UserDefaults
    private static let key = "me.lakko.ds.founded-unclaimed.v1"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func mark(journalIdHex: String) {
        var set = Set(defaults.stringArray(forKey: Self.key) ?? [])
        set.insert(journalIdHex)
        defaults.set(Array(set).sorted(), forKey: Self.key)
    }

    /// ``mark(journalIdHex:)`` for a journal of `accountIdB64` (its
    /// DeviceGroup, a channel it owns). The set is device-wide, but each mark
    /// is that account's data, and the wipe removes the account's marks: a
    /// mark made after the account's session ended (``AccountStoreGate``), or
    /// with no account named, is not written.
    public func mark(journalIdHex: String, accountIdB64: String?) {
        guard let accountIdB64, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        mark(journalIdHex: journalIdHex)
    }

    public func isMarked(journalIdHex: String) -> Bool {
        (defaults.stringArray(forKey: Self.key) ?? []).contains(journalIdHex)
    }

    /// Every marked journal id (``AccountLocalDataWipe`` drops the ones of the
    /// account it wipes).
    public func all() -> [String] {
        defaults.stringArray(forKey: Self.key) ?? []
    }

    public func clear(journalIdHex: String) {
        let rest = (defaults.stringArray(forKey: Self.key) ?? []).filter { $0 != journalIdHex }
        defaults.set(rest, forKey: Self.key)
    }
}

// MARK: - Claim on open

/// The owner's claim of a conversation group's journal, once per group per
/// session, without waiting for a `401`.
///
/// Live walk 2026-09-29: ADR-0057 migration 0006 dropped every earlier claim,
/// and a journal was re-claimed only by the owner's lazy claim, which runs
/// after a signed request is refused `401`. An UNCLAIMED journal never refuses
/// a signed read (it keeps the open posture), so a pre-existing group stayed
/// unclaimed for good, and every epoch registration in it was refused `401`:
/// no sweep could ever land and a new device was never admitted.
///
/// So the owner claims on open. The claim is idempotent for the same root at
/// the same key (`204`) and refused otherwise (`409`, or `401` when another
/// root holds the journal); neither refusal is answered with a reset — for a
/// conversation group a `409` usually means this device is behind.
///
/// ``ensure(journalIdHex:claim:)`` is what a registration awaits first, so an
/// owner's claim always precedes its first epoch registration of the session.
/// A settled outcome (claimed, claimed already, not the owner) is kept for the
/// session; a failed claim may be tried again after ``retryAfterFailureMs``.
@MainActor
public final class JournalClaimOnOpen {
    /// How long a failed claim waits before the next attempt. Chosen, not
    /// measured: the root probe's interval.
    public static let retryAfterFailureMs: Int64 = 60_000

    private var settled: [String: JournalOwnership.LazyClaimOutcome] = [:]
    private var failedAtMs: [String: Int64] = [:]
    private var inflight: [String: Task<JournalOwnership.LazyClaimOutcome, Never>] = [:]
    private let nowMs: () -> Int64

    public init(nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }) {
        self.nowMs = nowMs
    }

    /// Run `claim` for `journalIdHex` unless it already settled this session;
    /// a caller arriving while it runs awaits the same run. Returns the
    /// outcome, or `nil` when a failed claim is still inside its retry wait
    /// (nothing is sent then).
    @discardableResult
    public func ensure(
        journalIdHex: String,
        claim: @escaping @MainActor () async -> JournalOwnership.LazyClaimOutcome
    ) async -> JournalOwnership.LazyClaimOutcome? {
        if let outcome = settled[journalIdHex] { return outcome }
        if let task = inflight[journalIdHex] { return await task.value }
        if let failed = failedAtMs[journalIdHex] {
            let now = nowMs()
            if now >= failed, now < failed + Self.retryAfterFailureMs { return nil }
        }
        let task = Task { @MainActor in await claim() }
        inflight[journalIdHex] = task
        let outcome = await task.value
        inflight[journalIdHex] = nil
        if outcome == .failed {
            failedAtMs[journalIdHex] = nowMs()
        } else {
            failedAtMs[journalIdHex] = nil
            settled[journalIdHex] = outcome
        }
        return outcome
    }

    /// Whether the claim settled this session (claimed, claimed already, or
    /// not the owner).
    public func isSettled(journalIdHex: String) -> Bool { settled[journalIdHex] != nil }

    /// Forget one journal this device is no longer in (ADR-0087 section 9).
    public func forget(journalIdHex: String) {
        settled[journalIdHex] = nil
        failedAtMs[journalIdHex] = nil
        inflight[journalIdHex] = nil
    }

    /// Forget everything (a logout / account switch starts a new session).
    public func reset() {
        settled.removeAll()
        failedAtMs.removeAll()
        inflight.removeAll()
    }
}

// MARK: - Registrar seed store

/// This device's registrar seeds, per `(account, group)`, plus the owner's
/// record of which seed envelopes it delivered, the root-held markers, the
/// owed root handovers and the pending (received) root seeds.
///
/// A seed signs registrations for its journal, so it is stored like the push
/// routing secret: AES-256-GCM under a Keychain-resident wrap key
/// (``KeychainWrapKey``, `AfterFirstUnlockThisDeviceOnly`, no iCloud), the
/// sealed blob in app-private `UserDefaults.standard` (the NSE never commits,
/// so it never needs one). Never logged, never synced, wiped with the account.
///
/// The delivered record is not secret (account keys and ranks) and is plain.
public final class RegistrarSeedStore {
    public struct Seed: Equatable {
        public let seed: Data
        public let rank: DsRegistrarRank
    }

    private let defaults: UserDefaults
    private static let wrapKeyService = "me.lakko.ds-registrar-wrap.v1"
    private static let wrapKeyAccount = "wrap-key"
    private static let seedPrefix = "me.lakko.ds-registrar-seed.v1."
    private static let ledgerPrefix = "me.lakko.ds-registrar-appointed.v1."
    private static let owedPrefix = "me.lakko.ds-root-handover-owed.v1."
    private static let rootHeldPrefix = "me.lakko.ds-root-held.v1."
    private static let pendingRootPrefix = "me.lakko.ds-pending-root-seed.v1."
    private static let knownPrefix = "me.lakko.ds-registrar-known.v1."
    private static let seedLen = 32

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    // MARK: Seeds (registrar side)

    /// Keep a newly received seed; a newer envelope replaces the older one.
    public func put(accountIdB64: String, groupIdHex: String, seed: Data, rank: DsRegistrarRank) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard seed.count == Self.seedLen else { return }
        var plain = Data([rank == .admin ? 1 : 0])
        plain.append(seed)
        let key = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let sealed = try? AES.GCM.seal(plain, using: key).combined else { return }
        defaults.set(sealed, forKey: Self.seedPrefix + accountIdB64 + " " + groupIdHex)
    }

    public func get(accountIdB64: String, groupIdHex: String) -> Seed? {
        guard let sealed = defaults.data(forKey: Self.seedPrefix + accountIdB64 + " " + groupIdHex),
              let keyBytes = KeychainWrapKey.read(
                  service: Self.wrapKeyService, account: Self.wrapKeyAccount),
              let box = try? AES.GCM.SealedBox(combined: sealed),
              let plain = try? AES.GCM.open(box, using: SymmetricKey(data: keyBytes)),
              plain.count == 1 + Self.seedLen,
              let flag = plain.first, flag <= 1 else { return nil }
        return Seed(seed: Data(plain.dropFirst()), rank: flag == 1 ? .admin : .member)
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        defaults.removeObject(forKey: Self.seedPrefix + accountIdB64 + " " + groupIdHex)
    }

    // MARK: Delivered seed envelopes (owner side)

    /// Which devices the owner has sent their account's seed envelope to,
    /// leaf signature key (an account key where no leaf was known) -> rank
    /// (ADR-0057 amended 2026-10-01: per device, so a device that joined after
    /// the envelope was posted gets one too). Since the second security pass (finding 2) this record decides
    /// nothing about the DS: the owner converges the DS with a whole
    /// `registrars-set` from the live roster, and this only tracks which
    /// members still need an envelope.
    public func delivered(accountIdB64: String, groupIdHex: String) -> [Data: DsRegistrarRank] {
        let raw = defaults.dictionary(forKey: Self.ledgerPrefix + accountIdB64 + " " + groupIdHex)
            as? [String: String] ?? [:]
        var out: [Data: DsRegistrarRank] = [:]
        for (keyB64, rank) in raw {
            guard let key = Data(base64Encoded: keyB64) else { continue }
            out[key] = rank == "admin" ? .admin : .member
        }
        return out
    }

    public func saveDelivered(
        accountIdB64: String, groupIdHex: String, _ record: [Data: DsRegistrarRank]
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var raw: [String: String] = [:]
        for (key, rank) in record { raw[key.base64EncodedString()] = rank == .admin ? "admin" : "member" }
        defaults.set(raw, forKey: Self.ledgerPrefix + accountIdB64 + " " + groupIdHex)
    }

    /// Forget which envelopes were delivered on a journal: its root changed
    /// (every registrar seed changes with it).
    public func forgetDelivered(accountIdB64: String, groupIdHex: String) {
        defaults.removeObject(forKey: Self.ledgerPrefix + accountIdB64 + " " + groupIdHex)
    }

    // MARK: Known registrar keys (every member, ADR-0057 amended 2026-10-01)

    /// A registrar public key the owner appointed for one account, read from
    /// the clear header of a registrar-seed envelope (any addressee).
    public struct KnownRegistrar: Equatable {
        public let publicKey: Data
        public let rank: DsRegistrarRank

        public init(publicKey: Data, rank: DsRegistrarRank) {
            self.publicKey = publicKey
            self.rank = rank
        }
    }

    /// Account key -> the registrar key and rank the owner last appointed it
    /// at, on this journal: what an admin revokes when it commits that
    /// account's removal. Public keys and ranks only: plain `UserDefaults`.
    public func knownRegistrars(accountIdB64: String, groupIdHex: String) -> [Data: KnownRegistrar] {
        let raw = defaults.dictionary(
            forKey: Self.knownPrefix + accountIdB64 + " " + groupIdHex.lowercased())
            as? [String: String] ?? [:]
        var out: [Data: KnownRegistrar] = [:]
        for (accountB64, value) in raw {
            let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
            guard parts.count == 2, let account = Data(base64Encoded: accountB64),
                  let key = Data(base64Encoded: parts[1]), key.count == Self.seedLen else { continue }
            out[account] = KnownRegistrar(
                publicKey: key, rank: parts[0] == "admin" ? .admin : .member)
        }
        return out
    }

    /// Record (or replace: the newest envelope wins, a re-appointment or a new
    /// root changes the key) the registrar key appointed for `accountKey`.
    public func noteRegistrar(
        accountIdB64: String, groupIdHex: String, accountKey: Data, publicKey: Data,
        rank: DsRegistrarRank
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard publicKey.count == Self.seedLen else { return }
        let storeKey = Self.knownPrefix + accountIdB64 + " " + groupIdHex.lowercased()
        var raw = defaults.dictionary(forKey: storeKey) as? [String: String] ?? [:]
        raw[accountKey.base64EncodedString()] =
            (rank == .admin ? "admin" : "member") + ":" + publicKey.base64EncodedString()
        defaults.set(raw, forKey: storeKey)
    }

    // MARK: Root held (second security pass, finding 4)

    /// `true` when this account is known to hold the journal root: its claim
    /// answered 204, its takeover answered 204, or its probe did. Group ids
    /// only, nothing secret: plain `UserDefaults`.
    public func isRootHeld(accountIdB64: String, groupIdHex: String) -> Bool {
        (defaults.stringArray(forKey: Self.rootHeldPrefix + accountIdB64) ?? [])
            .contains(groupIdHex.lowercased())
    }

    public func markRootHeld(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var held = Set(defaults.stringArray(forKey: Self.rootHeldPrefix + accountIdB64) ?? [])
        guard held.insert(groupIdHex.lowercased()).inserted else { return }
        defaults.set(held.sorted(), forKey: Self.rootHeldPrefix + accountIdB64)
    }

    public func clearRootHeld(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var held = Set(defaults.stringArray(forKey: Self.rootHeldPrefix + accountIdB64) ?? [])
        guard held.remove(groupIdHex.lowercased()) != nil else { return }
        defaults.set(held.sorted(), forKey: Self.rootHeldPrefix + accountIdB64)
    }

    // MARK: Pending root seeds (new owner, finding 4)

    /// Keep the OLD root seed a handover delivered until the takeover lands
    /// (or answers 401), so a network failure or a restart retries it. Sealed
    /// under the same Keychain wrap key as the registrar seeds; never logged,
    /// never synced, never in the registrar-seed rows.
    public func putPendingRootSeed(accountIdB64: String, groupIdHex: String, seed: Data) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard seed.count == Self.seedLen else { return }
        let key = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let sealed = try? AES.GCM.seal(seed, using: key).combined else { return }
        defaults.set(sealed, forKey: Self.pendingRootPrefix + accountIdB64 + " " + groupIdHex.lowercased())
    }

    public func pendingRootSeed(accountIdB64: String, groupIdHex: String) -> Data? {
        guard let sealed = defaults.data(
                forKey: Self.pendingRootPrefix + accountIdB64 + " " + groupIdHex.lowercased()),
              let keyBytes = KeychainWrapKey.read(
                  service: Self.wrapKeyService, account: Self.wrapKeyAccount),
              let box = try? AES.GCM.SealedBox(combined: sealed),
              let plain = try? AES.GCM.open(box, using: SymmetricKey(data: keyBytes)),
              plain.count == Self.seedLen else { return nil }
        return plain
    }

    public func removePendingRootSeed(accountIdB64: String, groupIdHex: String) {
        defaults.removeObject(forKey: Self.pendingRootPrefix + accountIdB64 + " " + groupIdHex.lowercased())
    }

    /// The groups (lowercase hex) with a pending root seed.
    public func pendingRootSeedGroups(accountIdB64: String) -> [String] {
        let prefix = Self.pendingRootPrefix + accountIdB64 + " "
        return defaults.dictionaryRepresentation().keys
            .filter { $0.hasPrefix(prefix) }
            .map { String($0.dropFirst(prefix.count)) }
            .sorted()
    }

    // MARK: Owed root handovers (ADR-0057 S2 finding 1)

    /// The groups this account still owes a root handover for (lowercase hex
    /// group ids). Nothing secret: group ids only, so plain `UserDefaults`.
    public func owedHandovers(accountIdB64: String) -> Set<String> {
        Set(defaults.stringArray(forKey: Self.owedPrefix + accountIdB64) ?? [])
    }

    public func markHandoverOwed(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var owed = owedHandovers(accountIdB64: accountIdB64)
        guard owed.insert(groupIdHex.lowercased()).inserted else { return }
        defaults.set(owed.sorted(), forKey: Self.owedPrefix + accountIdB64)
    }

    public func clearHandoverOwed(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var owed = owedHandovers(accountIdB64: accountIdB64)
        guard owed.remove(groupIdHex.lowercased()) != nil else { return }
        if owed.isEmpty {
            defaults.removeObject(forKey: Self.owedPrefix + accountIdB64)
        } else {
            defaults.set(owed.sorted(), forKey: Self.owedPrefix + accountIdB64)
        }
    }

    /// Forget one journal this account is no longer in (ADR-0087 section 9):
    /// its seed, delivered record, known registrar keys, owed handover,
    /// root-held marker and pending root seed. A group this device comes back
    /// to appoints it again.
    public func forgetGroup(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let suffix = accountIdB64 + " " + groupIdHex
        defaults.removeObject(forKey: Self.seedPrefix + suffix)
        defaults.removeObject(forKey: Self.ledgerPrefix + suffix)
        defaults.removeObject(forKey: Self.pendingRootPrefix + suffix)
        defaults.removeObject(forKey: Self.knownPrefix + accountIdB64 + " " + groupIdHex.lowercased())
        clearRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        clearHandoverOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// Wipe every seed, delivered record, known registrar key, owed handover,
    /// root-held marker and pending root seed of `accountIdB64` (forget
    /// device).
    public func clear(accountIdB64: String) {
        for (key, _) in defaults.dictionaryRepresentation()
        where key.hasPrefix(Self.seedPrefix + accountIdB64 + " ")
            || key.hasPrefix(Self.ledgerPrefix + accountIdB64 + " ")
            || key.hasPrefix(Self.pendingRootPrefix + accountIdB64 + " ")
            || key.hasPrefix(Self.knownPrefix + accountIdB64 + " ") {
            defaults.removeObject(forKey: key)
        }
        defaults.removeObject(forKey: Self.owedPrefix + accountIdB64)
        defaults.removeObject(forKey: Self.rootHeldPrefix + accountIdB64)
    }
}
