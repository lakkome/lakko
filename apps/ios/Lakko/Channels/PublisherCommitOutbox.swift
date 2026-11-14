// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Durable record of a channel PublisherGroup commit (G-50, ADR-0090
/// section 3). Swift mirror of `apps/web/src/channels/publisher-commit-outbox.ts`.
///
/// ADR-0090: every PublisherGroup commit (a co-publisher approve, a rotation,
/// a publisher's removal, the admission or retirement of devices) is STAGED:
/// the group stays at epoch N holding the commit, and merges it only when the
/// Delivery Service took it (`201`, or `200` for the identical re-send);
/// a `409` abandons it (another owner device's commit won the epoch) and
/// nothing of it leaves the device. The reseal follows the merge, never the
/// stage, so a lost race never puts a seed only the loser holds in front of
/// the subscribers (C-90 proposed). An unanswered commit stays staged and
/// owed: every pass and every start sends it again until the DS answers.
///
/// ## Order — entry first, then the snapshot carrying the staged commit
///
/// The entry is written FIRST, then the provider snapshot holding the
/// staged commit, both fail-closed, before the commit is sent. "A staged
/// commit durable with no entry" is impossible by construction. The torn
/// state, an entry whose snapshot did not land, restores a group that holds
/// no staged commit and is still at N: the replay DISCARDS it
/// (``replayVerdict(_:restoredEpoch:staged:)``).
///
/// Each step marks itself done as it lands; the entry is dropped once none is
/// left. A replay runs the remaining steps again, each idempotent.
public struct PublisherCommitEntry: Codable, Equatable, Sendable {
    /// Which commit an entry records.
    public enum Kind: String, Codable, Sendable {
        case approve
        case rotation
        /// ADR-0087 section 15 (phase B) — the owner removes a publisher (or
        /// applies its step-down): every leaf of the account in one commit.
        /// `joinerKeyB64` holds the removed account key.
        case removal
        /// ADR-0090 section 2 — devices of an account that already publishes
        /// admitted and / or retired in one commit. `joinerKeyB64` holds the
        /// ACCOUNT key the Welcome goes to (when anything was admitted).
        case devices
        /// ADR-0064 section 7 (amendment of 2026-11-14) — this owner device
        /// enters the group by an external commit built from a saved
        /// GroupInfo. `proofEpoch` / `proofSeedB64` are the saved epoch's.
        case entry
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
    /// ADR-0090 — the DS answered the journal step with `409` or a definitive
    /// refusal: the staged commit was abandoned and nothing of it leaves the
    /// device (no Welcome, no appointment, no reseal). `nil` reads `false`.
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
        // ADR-0090 — the reseal before the Welcome: the admitted device then
        // reads the channel's epoch from the service at landing.
        case .approve: return [.journal, .reseal, .welcome, .appoint]
        case .rotation: return [.journal, .reseal]
        case .removal: return [.journal, .unappoint, .reseal]
        case .devices: return [.journal, .reseal, .welcome]
        case .entry: return [.journal, .reseal]
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
        /// The commit's staged state never became durable (a torn write, or
        /// the group is gone), or it was abandoned: drop it.
        case discard
        /// Run the remaining steps (the journal step first when the commit
        /// is still staged: the owed verdict).
        case replay
    }

    /// The verdict for an entry, given the epoch the restored group is at
    /// (`nil`: the group did not load at all) and whether it still holds a
    /// staged commit.
    public static func replayVerdict(
        _ entry: PublisherCommitEntry, restoredEpoch: UInt64?, staged: Bool = false
    ) -> ReplayVerdict {
        guard let restoredEpoch else { return .discard }
        if entry.journalLost == true { return .discard }
        if staged, !entry.done.contains(.journal) { return .replay }
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
        if kind == .devices || kind == .entry, commitB64 == nil { return false }
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
    /// ADR-0090 — whether the commit is already merged (a replay after the
    /// merge whose `done` mark did not land): the group holds no staged
    /// commit and is at or past the target epoch. The journal step is then
    /// done without a send.
    public var alreadyMerged: @MainActor () -> Bool
    /// ADR-0090 — the DS's verdict on the staged commit: `true` merges it
    /// (and persists), `false` abandons it (and persists). Throws when the
    /// group could not be written.
    public var adjudicate: @MainActor (_ accepted: Bool) throws -> Void
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
        alreadyMerged: @escaping @MainActor () -> Bool = { false },
        adjudicate: @escaping @MainActor (Bool) throws -> Void = { _ in },
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
        self.alreadyMerged = alreadyMerged
        self.adjudicate = adjudicate
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
    /// did land) for the next run. ADR-0090: the journal step sends the
    /// staged commit and merges it on acceptance; a lost commit (`409`, a
    /// definitive refusal) is abandoned and the entry finishes with no other
    /// step (no Welcome, no appointment, no reseal: C-90 proposed). An
    /// unanswered send throws and keeps the commit staged and owed.
    @MainActor
    public static func run(_ entry: PublisherCommitEntry, ports: PublisherCommitPorts) async throws {
        var current = entry
        for step in current.remainingSteps {
            switch step {
            case .journal:
                if ports.alreadyMerged() { break }
                if let commit = current.commitB64.flatMap({ Data(base64Encoded: $0) }) {
                    var proof: (epoch: UInt64, seed: Data)?
                    if let epoch = current.proofEpoch,
                       let seed = current.proofSeedB64.flatMap({ Data(base64Encoded: $0) }) {
                        proof = (epoch, seed)
                    }
                    do {
                        try await ports.commitToJournal(commit, proof)
                    } catch where ports.isLost(error) {
                        // Another owner device's commit took the epoch (or the
                        // DS refused this one): abandon, apply the winner on the
                        // next journal read, and send nothing of it.
                        NSLog("ADR-0090: PublisherGroup commit not taken by the journal (%@); abandoned",
                              current.kind.rawValue)
                        try ports.adjudicate(false)
                        current.journalLost = true
                        ports.finish(current)
                        return
                    }
                    NSLog("ADR-0090: PublisherGroup commit taken by the journal (%@); merged",
                          current.kind.rawValue)
                    try ports.adjudicate(true)
                }
            case .welcome:
                if let welcome = current.welcomeB64.flatMap({ Data(base64Encoded: $0) }),
                   let joiner = current.joinerKeyB64.flatMap({ Data(base64Encoded: $0) }) {
                    try await ports.deliverWelcome(welcome, joiner)
                }
            case .reseal:
                if !current.resealLanded(serverEpoch: try await ports.serverEpoch()) {
                    try await ports.reseal(current.serverTargetEpoch)
                }
            case .appoint:
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
