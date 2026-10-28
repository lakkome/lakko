// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0081 decision 2 — the DeviceGroup commit outbox.
///
/// Mirror of `apps/web/src/devices/device-group-commit.ts` and Android's
/// `DeviceGroupCommit.kt`.
///
/// A DeviceGroup commit (add a device by pairing or by the ADR-0049
/// confirmation, remove a device, enable attested removals, and the two
/// commits a rejoin admission makes) is built on a throwaway handle restored
/// from the durable snapshot at epoch N. Before this file the device then
/// registered N+1 on the DS, persisted N+1 and published the commit, and
/// three failures stranded the account: a crash between the registration and
/// the persist (the DS held N+1, every device N, the commit nowhere), a failed
/// publish after the persist (nothing retried it), and a persist failure that
/// was swallowed (the commit published while the device stayed at N).
///
/// The order now, with the DeviceGroup's current snapshot staying at N until
/// the DS has answered:
///
///   1. build the commit on a throwaway handle (the caller);
///   2. write an entry — the journal row (the ADR-0015 envelope, so the
///      removal attestation rides in it), the Welcome and its rendezvous, the
///      N+1 epoch and its access key, the N+1 snapshot — BESIDE the N
///      snapshot (``DeviceGroupSnapshotStore/PendingKind/commit``). A failed
///      write aborts: nothing has left the device;
///   3. `POST /ds/group/<hex>/commit`: the N+1 registration (signed by the
///      account's root) and the row in one transaction (decision 1);
///   4. on success make the entry's N+1 snapshot current (fail-closed), tell
///      the sync runtime its handle is stale, deliver the Welcome, delete the
///      entry;
///   5. on `409` delete the entry: the device stays at N and applies the
///      winner's commit from the journal;
///   6. on any other failure, or a restart, the entry is retried
///      (``resendOwed(ports:)``). The re-send is idempotent on the DS, so a
///      crash after the DS answered completes on the retry.
///
/// An entry is re-sent before any new DeviceGroup commit is built, so two
/// commits of one device never interleave: the callers hold
/// ``DeviceGroupCommitLock`` across "re-send what is owed, build, run". At
/// most one entry exists per account.
///
/// ## Why step 4 checks the current epoch
///
/// A retry may find the N+1 snapshot already current (the earlier run made it
/// current and then failed on the Welcome). Writing the entry's snapshot again
/// would then put back an N+1 state the sync runtime has since moved on from —
/// older sender ratchet generations, so this device would seal two messages
/// under one key and nonce. So the snapshot is made current only while the
/// durable state is still below the entry's epoch.
///
/// Everything that decides is here; the network, the store and the MLS calls
/// are ports, so the host tests drive each failure path.
public struct DeviceGroupCommitEntry: Codable, Equatable {
    /// Which DeviceGroup change an entry carries.
    public enum Kind: String, Codable, Equatable {
        case add
        case remove
        case policy
        case rejoinRemove = "rejoin-remove"
        case rejoinAdd = "rejoin-add"
        /// ADR-0081 amendment — the previous leaf removed and the new one
        /// added in one commit (the rejoin under the attested-removal policy).
        case rejoinReplace = "rejoin-replace"
    }

    public var v: Int
    public var kind: Kind
    /// The DeviceGroup journal id, standard base64.
    public var journalIdB64: String
    /// The epoch the commit moves the DeviceGroup to (decimal `u64`).
    public var epoch: String
    /// That epoch's access verifying key (32 bytes), standard base64.
    public var epochKeyB64: String
    /// The journal row: the ADR-0015 envelope around the commit.
    public var rowB64: String
    /// The Welcome for an added device, and the rendezvous it goes to.
    public var welcomeB64: String?
    public var welcomeRendezvousB64: String?
    /// The DeviceIdentity provider snapshot at the commit's epoch.
    public var nextSnapshotB64: String
    public var createdAtMs: Int64

    public init(
        kind: Kind,
        journalIdB64: String,
        epoch: UInt64,
        epochKeyB64: String,
        rowB64: String,
        welcomeB64: String? = nil,
        welcomeRendezvousB64: String? = nil,
        nextSnapshotB64: String,
        createdAtMs: Int64
    ) {
        self.v = 1
        self.kind = kind
        self.journalIdB64 = journalIdB64
        self.epoch = String(epoch)
        self.epochKeyB64 = epochKeyB64
        self.rowB64 = rowB64
        self.welcomeB64 = welcomeB64
        self.welcomeRendezvousB64 = welcomeRendezvousB64
        self.nextSnapshotB64 = nextSnapshotB64
        self.createdAtMs = createdAtMs
    }

    /// The epoch as a number, `nil` when the stored text is not one.
    public var epochValue: UInt64? { UInt64(epoch) }

    /// Encode for the store.
    public func encoded() throws -> Data {
        try JSONEncoder().encode(self)
    }

    /// Decode a stored entry, or `nil` when it is not one this build wrote (a
    /// row it cannot read is treated as absent and dropped by the caller).
    public static func decode(_ bytes: Data) -> DeviceGroupCommitEntry? {
        guard let entry = try? JSONDecoder().decode(DeviceGroupCommitEntry.self, from: bytes),
              entry.v == 1,
              entry.epochValue != nil,
              Data(base64Encoded: entry.journalIdB64) != nil,
              Data(base64Encoded: entry.epochKeyB64) != nil,
              Data(base64Encoded: entry.rowB64) != nil,
              Data(base64Encoded: entry.nextSnapshotB64) != nil,
              (entry.welcomeB64 == nil) == (entry.welcomeRendezvousB64 == nil)
        else { return nil }
        return entry
    }
}

/// What one commit attempt came to.
public enum DeviceGroupCommitOutcome: Equatable {
    /// The commit is on the journal and its epoch is this device's.
    case landed(messageId: Int64)
    /// Another commit owns the epoch: nothing of this one is on the DS, the
    /// entry is gone, the device stays where it was.
    case lost
}

/// The ports the outbox runs through.
@MainActor
public struct DeviceGroupCommitPorts {
    public var saveEntry: (DeviceGroupCommitEntry) throws -> Void
    public var loadEntry: () -> DeviceGroupCommitEntry?
    public var deleteEntry: () -> Void
    /// The proof signer of the durable state's epoch, or `nil` when no
    /// durable DeviceGroup state can be loaded.
    public var durableProof: () -> DsRequestSigner?
    /// `POST .../commit` with the root's statement for the entry, proven by
    /// the signer; throws ``DsStatementError/conflict`` on a lost epoch.
    public var commit: (DeviceGroupCommitEntry, DsRequestSigner) async throws -> DsCommitAnswer
    /// Write the entry's snapshot as the current DeviceGroup state. Throws on
    /// a failed write.
    public var makeCurrent: (DeviceGroupCommitEntry) throws -> Void
    /// The DeviceGroup changed through another handle: the sync runtime
    /// reloads. Never throws.
    public var announce: (DeviceGroupCommitEntry) -> Void
    /// Deliver the entry's Welcome to its rendezvous. Throws on failure.
    public var deliverWelcome: (DeviceGroupCommitEntry) async throws -> Void

    public init(
        saveEntry: @escaping (DeviceGroupCommitEntry) throws -> Void,
        loadEntry: @escaping () -> DeviceGroupCommitEntry?,
        deleteEntry: @escaping () -> Void,
        durableProof: @escaping () -> DsRequestSigner?,
        commit: @escaping (DeviceGroupCommitEntry, DsRequestSigner) async throws -> DsCommitAnswer,
        makeCurrent: @escaping (DeviceGroupCommitEntry) throws -> Void,
        announce: @escaping (DeviceGroupCommitEntry) -> Void,
        deliverWelcome: @escaping (DeviceGroupCommitEntry) async throws -> Void
    ) {
        self.saveEntry = saveEntry
        self.loadEntry = loadEntry
        self.deleteEntry = deleteEntry
        self.durableProof = durableProof
        self.commit = commit
        self.makeCurrent = makeCurrent
        self.announce = announce
        self.deliverWelcome = deliverWelcome
    }
}

/// The outbox's runner.
public enum DeviceGroupCommitOutbox {

    /// Refused: the proof is not of the epoch the commit leaves.
    public struct ProofEpochMismatch: Error, Equatable {}

    /// Steps 2-5 for a commit just built: write the entry beside the N
    /// snapshot (a failure aborts with nothing sent), then send it. `proof`
    /// is the signer of epoch N, taken from the handle the commit was built
    /// on BEFORE the create call. Throws on any failure other than a lost
    /// epoch; the entry then stays and ``resendOwed(ports:)`` finishes it.
    @MainActor
    public static func run(
        _ entry: DeviceGroupCommitEntry,
        proof: DsRequestSigner,
        ports: DeviceGroupCommitPorts
    ) async throws -> DeviceGroupCommitOutcome {
        guard let epoch = entry.epochValue, proof.epoch() &+ 1 == epoch else {
            throw ProofEpochMismatch()
        }
        try ports.saveEntry(entry)
        return try await finish(entry, proof: proof, ports: ports)
    }

    /// Step 6 — finish the entry a previous run left, if any, before anything
    /// else touches the DeviceGroup. `nil` when there was none. Throws when
    /// the DS cannot be reached or a write fails (the entry stays).
    @MainActor
    public static func resendOwed(ports: DeviceGroupCommitPorts) async throws -> DeviceGroupCommitOutcome? {
        guard let entry = ports.loadEntry() else { return nil }
        guard let proof = ports.durableProof() else {
            // No durable DeviceGroup to prove from: the state this entry was
            // built on is gone (deleted for a rejoin). Nothing can make it
            // current; drop it.
            ports.deleteEntry()
            return .lost
        }
        return try await finish(entry, proof: proof, ports: ports)
    }

    @MainActor
    private static func finish(
        _ entry: DeviceGroupCommitEntry,
        proof: DsRequestSigner,
        ports: DeviceGroupCommitPorts
    ) async throws -> DeviceGroupCommitOutcome {
        let answer: DsCommitAnswer
        do {
            answer = try await ports.commit(entry, proof)
        } catch DsStatementError.conflict {
            // Step 5: another commit owns the epoch. Whether or not this
            // entry's snapshot was ever made current, deleting it is right:
            // below the epoch the device applies the winner's commit; at or
            // above it the entry's work is already durable.
            ports.deleteEntry()
            return .lost
        }
        // Step 4: the commit is on the journal. Make its epoch this device's,
        // unless a previous run already did (see the type doc).
        if let epoch = entry.epochValue, proof.epoch() < epoch {
            try ports.makeCurrent(entry)
        }
        ports.announce(entry)
        if entry.welcomeB64 != nil, entry.welcomeRendezvousB64 != nil {
            try await ports.deliverWelcome(entry)
        }
        ports.deleteEntry()
        return .landed(messageId: answer.id)
    }
}

/// Serialisation of the device's DeviceGroup commits: a commit path runs
/// "re-send what is owed, build, run" after every DeviceGroup commit of the
/// same account started before it (serializing, never coalescing), so a
/// second commit is never built on an epoch a first one is still taking.
///
/// A stored task chain, as ``ChatRepository``'s commit lock: `@MainActor`
/// releases isolation at every `await`, so isolation alone does not hold a
/// commit across its network round trip.
@MainActor
public final class DeviceGroupCommitLock {
    public static let shared = DeviceGroupCommitLock()

    private var tails: [String: Task<Void, Never>] = [:]

    public init() {}

    public func with<T>(
        accountIdB64: String,
        _ body: @escaping @MainActor () async throws -> T
    ) async throws -> T {
        let prior = tails[accountIdB64]
        let task = Task { @MainActor () throws -> T in
            await prior?.value
            return try await body()
        }
        let tail = Task { @MainActor in _ = try? await task.value }
        tails[accountIdB64] = tail
        defer {
            // Keep the map small: the last waiter clears its own entry.
            Task { @MainActor [weak self] in
                await tail.value
                if self?.tails[accountIdB64] == tail { self?.tails[accountIdB64] = nil }
            }
        }
        return try await task.value
    }
}
