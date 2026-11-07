// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Durable record of a channel PublisherGroup commit (G-50, the ADR-0040
/// treatment the chat groups have). Swift mirror of
/// `apps/web/src/channels/publisher-commit-outbox.ts`.
///
/// A PublisherGroup commit (a co-publisher approve, a rotation) eager-merges
/// epoch N -> N+1 in the channel provider (``ChannelPublisherIdentity``). Its
/// effects then leave the device in steps: the N+1 epoch key is registered on
/// the Delivery Service and the Add commit published to the journal, the
/// joiner's Welcome is written to its mailbox (approve), every subscriber is
/// resealed to the N+1 seed and the channels service moves to the next epoch,
/// and the newcomer's co-publisher key is appointed (approve).
///
/// ## Order — entry first, then the N+1 snapshot, then the steps
///
/// iOS has no transaction spanning the entry and the provider snapshot, so
/// this takes ADR-0040's second sanctioned ordering, the one the iOS chat
/// outbox (`Chat/CommitOutbox.swift`) takes: the entry is written FIRST, then
/// the N+1 provider snapshot, both fail-closed, both before any step runs.
/// "N+1 durable with no entry" is impossible by construction. The one torn
/// state, an entry whose snapshot did not land, is a commit none of whose
/// steps ran, so the restart replay DISCARDS it (``replayVerdict(_:restoredEpoch:)``).
///
/// Each step marks itself done as it lands; the entry is dropped once none is
/// left. A replay runs the remaining steps again, each idempotent: the DS
/// journal drops a duplicate commit, a second Welcome for a group the joiner
/// is already in is skipped by it, the reseal targets the epoch recorded in
/// the entry and counts as done once the service is at or past it, and an
/// appointment is an upsert.
///
/// ## What an entry holds
///
/// The commit and the Welcome (public MLS artefacts the DS stores anyway),
/// the joiner's key and the epochs. The joiner's `reply` grant is NOT
/// recorded here (web records it because it holds the grant in memory only):
/// ``ReachCenter`` keeps it in the Keychain. Entries live in one
/// device-key-encrypted record per account (``PublisherCommitStore``).
public struct PublisherCommitEntry: Codable, Equatable, Sendable {
    /// Which commit an entry records.
    public enum Kind: String, Codable, Sendable {
        case approve
        case rotation
        /// ADR-0087 section 15 (phase B) — the owner removes a publisher (or
        /// applies its step-down): every leaf of the account in one commit.
        /// `joinerKeyB64` holds the removed account key.
        case removal
    }

    /// The steps of an entry, in the order they run.
    public enum Step: String, Codable, Sendable {
        case journal
        case welcome
        case reseal
        case appoint
        /// Removal: revoke the removed publisher's appointment.
        case unappoint
    }

    /// Random hex id.
    public let id: String
    public let kind: Kind
    /// 32-char hex channel id.
    public let channelIdHex: String
    /// 64-char hex PublisherGroup id (the DS journal).
    public let groupIdHex: String
    /// The MLS epoch the commit moved the group to.
    public let targetEpoch: UInt64
    /// The channels-service epoch the reseal binds the envelopes to.
    public let serverTargetEpoch: UInt32
    /// Approve: standard base64 of the Add commit for the journal.
    public let commitB64: String?
    /// Approve: standard base64 of the joiner's Welcome.
    public let welcomeB64: String?
    /// Approve: standard base64 of the joiner's signature (= account) key.
    public let joinerKeyB64: String?
    /// ADR-0081 — the epoch the commit LEFT, whose key proves the commit
    /// request. The commit merged eagerly, so after a restart the group holds
    /// only the N+1 key, and the DS takes a commit only under a proof of N
    /// (an identical re-send under any retained key); with the seed a replay
    /// always learns the answer: appended, the original row, or `409`.
    public let proofEpoch: UInt64?
    /// ADR-0081 — standard base64 of that epoch's 32-byte access seed. Lives
    /// only in this device-key-encrypted record and goes with it.
    public let proofSeedB64: String?
    /// ADR-0081 — the DS answered the journal step with `409` or a definitive
    /// refusal: nothing of the commit is on the journal, so an approve
    /// delivers no Welcome and appoints nobody (the reseal still runs: the
    /// owner's posts are sealed under the merged seed). `nil` reads `false`.
    public var journalLost: Bool?
    /// The steps that have landed.
    public var done: [Step]

    public init(
        id: String = PublisherCommitEntry.newId(),
        kind: Kind,
        channelIdHex: String,
        groupIdHex: String,
        targetEpoch: UInt64,
        serverTargetEpoch: UInt32,
        commitB64: String? = nil,
        welcomeB64: String? = nil,
        joinerKeyB64: String? = nil,
        proofEpoch: UInt64? = nil,
        proofSeedB64: String? = nil,
        journalLost: Bool? = nil,
        done: [Step] = []
    ) {
        self.id = id
        self.kind = kind
        self.channelIdHex = channelIdHex
        self.groupIdHex = groupIdHex
        self.targetEpoch = targetEpoch
        self.serverTargetEpoch = serverTargetEpoch
        self.commitB64 = commitB64
        self.welcomeB64 = welcomeB64
        self.joinerKeyB64 = joinerKeyB64
        self.proofEpoch = proofEpoch
        self.proofSeedB64 = proofSeedB64
        self.journalLost = journalLost
        self.done = done
    }

    /// The steps an entry of `kind` runs, in order.
    public static func steps(of kind: Kind) -> [Step] {
        switch kind {
        case .approve: return [.journal, .welcome, .reseal, .appoint]
        case .rotation: return [.journal, .reseal]
        case .removal: return [.journal, .unappoint, .reseal]
        }
    }

    /// The steps still to run, in order.
    public var remainingSteps: [Step] {
        Self.steps(of: kind).filter { !done.contains($0) }
    }

    /// The entry with `step` marked done (once).
    public func withStepDone(_ step: Step) -> PublisherCommitEntry {
        guard !done.contains(step) else { return self }
        var next = self
        next.done.append(step)
        return next
    }

    /// What a restart does with an entry.
    public enum ReplayVerdict: Equatable, Sendable {
        /// The commit's N+1 never became durable (a torn write, or the group
        /// is gone): none of its steps ran, drop it.
        case discard
        /// Run the remaining steps.
        case replay
    }

    /// The verdict for an entry, given the epoch the restored group is at
    /// (`nil`: the group did not load at all).
    public static func replayVerdict(_ entry: PublisherCommitEntry, restoredEpoch: UInt64?) -> ReplayVerdict {
        guard let restoredEpoch else { return .discard }
        return restoredEpoch < entry.targetEpoch ? .discard : .replay
    }

    /// Whether the reseal of an entry has already landed: the channels
    /// service is at or past the epoch the entry resealed to.
    public func resealLanded(serverEpoch: UInt32) -> Bool {
        serverEpoch >= serverTargetEpoch
    }

    /// A fresh random 16-byte hex id.
    public static func newId() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Whether the entry's fields are well formed (a malformed entry read
    /// back is dropped alone).
    public var isWellFormed: Bool {
        func isHex(_ s: String, _ len: Int) -> Bool {
            s.count == len && s.allSatisfy { $0.isHexDigit && !$0.isUppercase }
        }
        guard isHex(id, 32), isHex(channelIdHex, 32), isHex(groupIdHex, 64) else { return false }
        if kind == .approve, commitB64 == nil || welcomeB64 == nil || joinerKeyB64 == nil { return false }
        if kind == .removal, commitB64 == nil || joinerKeyB64 == nil { return false }
        return true
    }
}

/// What running the steps of one entry needs; injected so the order is
/// testable without a network.
public struct PublisherCommitPorts {
    /// ADR-0081 decision 1 — register the group's CURRENT epoch key (the N+1
    /// one) with the channel's root AND append `commit` to the group journal,
    /// in one request (`POST .../commit`), proven under the recorded key of
    /// the epoch the commit left (`proof`; `nil` on an entry that kept none).
    /// A re-send of the same commit is idempotent on the DS.
    public var commitToJournal: @MainActor (_ commit: Data, _ proof: (epoch: UInt64, seed: Data)?) async throws -> Void
    /// ADR-0081 — whether a failed journal step is lost (`409`, a definitive
    /// refusal): nothing of the commit is or will be on the DS.
    public var isLost: (Error) -> Bool
    /// Write the Welcome to the joiner's mailbox.
    public var deliverWelcome: @MainActor (_ welcome: Data, _ joinerKey: Data) async throws -> Void
    /// The channels service's current epoch.
    public var serverEpoch: @MainActor () async throws -> UInt32
    /// Reseal the group's CURRENT seed to every subscriber and move the
    /// service to `targetEpoch` (stage + commit).
    public var reseal: @MainActor (_ targetEpoch: UInt32) async throws -> Void
    /// Appoint the joiner's co-publisher key. Never fails the entry.
    public var appoint: @MainActor (_ joinerKey: Data) async throws -> Void
    /// Revoke a removed publisher's appointment. Never fails the entry.
    public var unappoint: @MainActor (_ accountKey: Data) async throws -> Void
    /// Durably record `entry` (a step landed).
    public var save: @MainActor (_ entry: PublisherCommitEntry) throws -> Void
    /// The entry is finished: drop it.
    public var finish: @MainActor (_ entry: PublisherCommitEntry) -> Void

    public init(
        commitToJournal: @escaping @MainActor (Data, (epoch: UInt64, seed: Data)?) async throws -> Void,
        isLost: @escaping (Error) -> Bool = DsCommitFailure.isLost,
        deliverWelcome: @escaping @MainActor (Data, Data) async throws -> Void,
        serverEpoch: @escaping @MainActor () async throws -> UInt32,
        reseal: @escaping @MainActor (UInt32) async throws -> Void,
        appoint: @escaping @MainActor (Data) async throws -> Void,
        unappoint: @escaping @MainActor (Data) async throws -> Void = { _ in },
        save: @escaping @MainActor (PublisherCommitEntry) throws -> Void,
        finish: @escaping @MainActor (PublisherCommitEntry) -> Void
    ) {
        self.commitToJournal = commitToJournal
        self.isLost = isLost
        self.deliverWelcome = deliverWelcome
        self.serverEpoch = serverEpoch
        self.reseal = reseal
        self.appoint = appoint
        self.unappoint = unappoint
        self.save = save
        self.finish = finish
    }
}

/// The runner of a recorded PublisherGroup commit.
public enum PublisherCommitRunner {
    /// Run the remaining steps of `entry` in order, recording each as it
    /// lands. Throws the first step's failure, leaving the entry (with what
    /// did land) for the next run. A rotation's journal step that failed for
    /// an unknown reason (the network) is the exception: the other steps run
    /// and the entry is kept, not finished, so the next replay sends the
    /// journal step again.
    @MainActor
    public static func run(_ entry: PublisherCommitEntry, ports: PublisherCommitPorts) async throws {
        var current = entry
        var journalDeferred = false
        stepLoop: for step in current.remainingSteps {
            switch step {
            case .journal:
                if let commit = current.commitB64.flatMap({ Data(base64Encoded: $0) }) {
                    var proof: (epoch: UInt64, seed: Data)?
                    if let epoch = current.proofEpoch,
                       let seed = current.proofSeedB64.flatMap({ Data(base64Encoded: $0) }) {
                        proof = (epoch, seed)
                    }
                    do {
                        try await ports.commitToJournal(commit, proof)
                    } catch where ports.isLost(error) {
                        // ADR-0081 — nothing of this commit is or will be on
                        // the DS (a co-publisher's rotation is refused like
                        // this: its root is not the journal's).
                        NSLog("ADR-0081: PublisherGroup commit not taken by the journal (%@)",
                              current.kind.rawValue)
                        current.journalLost = true
                    } catch {
                        guard current.kind == .rotation else { throw error }
                        // The subscribers' reseal does not wait on the DS.
                        NSLog("ADR-0081: PublisherGroup rotation commit deferred")
                        journalDeferred = true
                        continue stepLoop
                    }
                }
            case .welcome:
                if current.journalLost == true { break }
                if let welcome = current.welcomeB64.flatMap({ Data(base64Encoded: $0) }),
                   let joiner = current.joinerKeyB64.flatMap({ Data(base64Encoded: $0) }) {
                    try await ports.deliverWelcome(welcome, joiner)
                }
            case .reseal:
                if !current.resealLanded(serverEpoch: try await ports.serverEpoch()) {
                    try await ports.reseal(current.serverTargetEpoch)
                }
            case .appoint:
                if current.journalLost == true { break }
                if let joiner = current.joinerKeyB64.flatMap({ Data(base64Encoded: $0) }) {
                    do {
                        try await ports.appoint(joiner)
                    } catch {
                        // An appointment is retried by the next co-publisher
                        // open; it never holds the entry.
                        NSLog("ADR-0059: appointing the co-publisher key after an approve failed")
                    }
                }
            case .unappoint:
                if let removed = current.joinerKeyB64.flatMap({ Data(base64Encoded: $0) }) {
                    do {
                        try await ports.unappoint(removed)
                    } catch {
                        // The service's key list is the posting gate; the
                        // removal itself already took the person's seed.
                        NSLog("ADR-0087: revoking a removed publisher's appointment failed")
                    }
                }
            }
            current = current.withStepDone(step)
            try ports.save(current)
        }
        if journalDeferred { return }
        ports.finish(current)
    }
}

/// The per-account store of recorded PublisherGroup commits: one
/// device-key-encrypted record (`<account>.publisher-commits` in
/// ``IdentitySnapshotStore``) holding the JSON array of entries. Main-actor
/// confined, so a read-modify-write is never interleaved.
@MainActor
public final class PublisherCommitStore {
    /// The process-wide instance.
    public static let shared = PublisherCommitStore()

    /// The record suffix.
    public nonisolated static let scope = ".publisher-commits"

    private let store: IdentitySnapshotStore

    init(store: IdentitySnapshotStore = IdentitySnapshotStore(scope: PublisherCommitStore.scope)) {
        self.store = store
    }

    /// Every well-formed entry of the account. An unreadable record reads as
    /// empty; a malformed entry is dropped alone.
    public func entries(accountIdB64: String) -> [PublisherCommitEntry] {
        guard let blob = store.getSnapshot(accountIdB64: accountIdB64),
              let decoded = try? JSONDecoder().decode([PublisherCommitEntry].self, from: blob)
        else { return [] }
        return decoded.filter(\.isWellFormed)
    }

    /// Insert or replace `entry`, durably. Throws when it was not written
    /// (fail-closed: a commit whose entry is not on disk is never published).
    public func put(accountIdB64: String, _ entry: PublisherCommitEntry) throws {
        var all = entries(accountIdB64: accountIdB64).filter { $0.id != entry.id }
        all.append(entry)
        try store.putSnapshotDurable(accountIdB64: accountIdB64, blob: try JSONEncoder().encode(all))
    }

    /// Drop the entry `id`. Best effort: a lost removal replays the entry,
    /// whose steps are all done or idempotent.
    public func remove(accountIdB64: String, id: String) {
        let rest = entries(accountIdB64: accountIdB64).filter { $0.id != id }
        if rest.isEmpty {
            store.deleteSnapshot(accountIdB64: accountIdB64)
            return
        }
        guard let blob = try? JSONEncoder().encode(rest) else { return }
        try? store.putSnapshotDurable(accountIdB64: accountIdB64, blob: blob)
    }

    /// Forget device: drop every entry of the account.
    public nonisolated static func deleteAll(accountIdB64: String) {
        IdentitySnapshotStore(scope: scope).deleteSnapshot(accountIdB64: accountIdB64)
    }
}
