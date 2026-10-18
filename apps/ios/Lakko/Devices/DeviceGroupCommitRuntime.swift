// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

extension Notification.Name {
    /// ADR-0081 — a DeviceGroup commit of this device landed and its epoch was
    /// made current through a handle of its own (``DeviceGroupCommits``). The
    /// sync runtime reloads its long-lived handle from the snapshot.
    /// `userInfo["accountIdB64"]` names the account.
    static let lakkoDeviceGroupChangedHere = Notification.Name("lakko.deviceGroupChangedHere")
}

/// ADR-0081 decision 2 on iOS: the ports of the DeviceGroup commit outbox
/// (``DeviceGroupCommitOutbox``) over the snapshot store, the DS client and the
/// DeviceGroup, and the one entry point every DeviceGroup commit path goes
/// through (``commitChange(accountIdBytes:accountIdB64:ds:build:after:)``).
///
/// Mirror of `apps/web/src/devices/device-group-commit-runtime.ts`.
public enum DeviceGroupCommits {

    /// What a commit path hands the outbox once it has built its commit on
    /// the throwaway handle (which is now at the commit's epoch in memory).
    public struct Built {
        public let kind: DeviceGroupCommitEntry.Kind
        /// The journal row: the ADR-0015 envelope around the commit.
        public let row: Data
        /// An added device's Welcome and the rendezvous it is delivered to.
        public let welcome: Data?
        public let welcomeRendezvous: Data?

        public init(kind: DeviceGroupCommitEntry.Kind, row: Data, welcome: Data? = nil, welcomeRendezvous: Data? = nil) {
            self.kind = kind
            self.row = row
            self.welcome = welcome
            self.welcomeRendezvous = welcomeRendezvous
        }
    }

    /// The ports of the outbox for one account.
    @MainActor
    public static func ports(
        accountIdBytes: Data,
        accountIdB64: String,
        ds: DsClient,
        store: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) -> DeviceGroupCommitPorts {
        DeviceGroupCommitPorts(
            saveEntry: { entry in
                try store.putPending(accountIdB64: accountIdB64, kind: .commit, blob: try entry.encoded())
            },
            loadEntry: {
                guard let bytes = store.getPending(accountIdB64: accountIdB64, kind: .commit) else { return nil }
                guard let entry = DeviceGroupCommitEntry.decode(bytes) else {
                    // A row this build cannot read cannot be finished; dropped.
                    store.deletePending(accountIdB64: accountIdB64, kind: .commit)
                    return nil
                }
                return entry
            },
            deleteEntry: {
                store.deletePending(accountIdB64: accountIdB64, kind: .commit)
            },
            durableProof: {
                guard let seed = try? DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64),
                      let ensured = try? DeviceGroupLifecycle.ensureDeviceGroup(
                        accountIdBytes: accountIdBytes, accountIdB64: accountIdB64,
                        deviceIdentitySeed: seed, snapshotStore: store)
                else { return nil }
                return try? ensured.group.dsAccessSigner(identity: ensured.identity)
            },
            commit: { entry, proof in
                guard let journalId = Data(base64Encoded: entry.journalIdB64),
                      let epochKey = Data(base64Encoded: entry.epochKeyB64),
                      let row = Data(base64Encoded: entry.rowB64),
                      let epoch = entry.epochValue else {
                    throw DsStatementError.malformed("a DeviceGroup commit entry does not decode")
                }
                guard let root = DeviceGroupDsAccess.rootSigner(accountIdB64: accountIdB64, groupId: journalId) else {
                    throw DsStatementError.unauthorized
                }
                return try await JournalOwnership.commitEpoch(
                    authority: .root(root), journalId: journalId, epoch: epoch, epochKey: epochKey,
                    revoke: [], row: row, proof: proof, ds: ds)
            },
            makeCurrent: { entry in
                guard let snapshot = Data(base64Encoded: entry.nextSnapshotB64) else {
                    throw DsStatementError.malformed("a DeviceGroup commit entry carries no snapshot")
                }
                try store.putSnapshotDurable(accountIdB64: accountIdB64, blob: snapshot)
            },
            announce: { _ in
                NotificationCenter.default.post(
                    name: .lakkoDeviceGroupChangedHere, object: nil,
                    userInfo: ["accountIdB64": accountIdB64])
            },
            deliverWelcome: { entry in
                guard let welcome = entry.welcomeB64.flatMap({ Data(base64Encoded: $0) }),
                      let rendezvous = entry.welcomeRendezvousB64.flatMap({ Data(base64Encoded: $0) })
                else { return }
                try await WelcomeCourier.publishWelcome(
                    rendezvousPseudonym: rendezvous, welcomeBytes: welcome, ds: ds)
            }
        )
    }

    /// Step 6 for the account, under the commit lock: finish an entry a
    /// previous run left. `nil` when there was none. Throws when it could not
    /// be finished now (the entry stays).
    @MainActor
    @discardableResult
    public static func resendOwed(
        accountIdBytes: Data,
        accountIdB64: String,
        ds: DsClient = DsClient()
    ) async throws -> DeviceGroupCommitOutcome? {
        try await DeviceGroupCommitLock.shared.with(accountIdB64: accountIdB64) {
            try await DeviceGroupCommitOutbox.resendOwed(
                ports: ports(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64, ds: ds))
        }
    }

    /// Every DeviceGroup commit path: under the account's commit lock, finish
    /// an owed entry first (a failure there stops this commit: two commits of
    /// one device never interleave), load a throwaway handle at the durable
    /// epoch N (never founding), claim on open, let `build` create the commit
    /// on it, then run the outbox (``DeviceGroupCommitOutbox/run(_:proof:ports:)``):
    /// the entry beside the N snapshot, one `POST .../commit`, N+1 made
    /// current, the Welcome, the entry deleted.
    ///
    /// `after` runs on the same throwaway handle (now at N+1) only when the
    /// commit landed — the pairing flow seals its history, recovery and state
    /// bundles there. Its failure does not undo the commit (it is already on
    /// the journal); it propagates.
    ///
    /// Returns `nil` when `build` decided there is nothing to commit. Throws
    /// when the build refuses, when the entry cannot be written (nothing was
    /// sent), and when the DS cannot be reached (the entry stays and is
    /// finished later). A lost epoch is not an error: the outcome is
    /// ``DeviceGroupCommitOutcome/lost`` and `after` is not run.
    @MainActor
    public static func commitChange<T>(
        accountIdBytes: Data,
        accountIdB64: String,
        ds: DsClient = DsClient(),
        build: @escaping @MainActor (DeviceGroupLifecycle.Ensured) throws -> Built?,
        after: @escaping @MainActor (DeviceGroupLifecycle.Ensured) async throws -> T
    ) async throws -> (outcome: DeviceGroupCommitOutcome, value: T?)? {
        try await DeviceGroupCommitLock.shared.with(accountIdB64: accountIdB64) {
            let ports = ports(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64, ds: ds)
            _ = try await DeviceGroupCommitOutbox.resendOwed(ports: ports)
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: accountIdBytes, accountIdB64: accountIdB64, deviceIdentitySeed: seed)
            // ADR-0053 point 2 — claim-on-open (idempotent; also the claim of
            // a DeviceGroup founded on this device). Second security pass,
            // finding 3 — a founded DeviceGroup whose claim did not land
            // commits nothing.
            await DeviceGroupDsAccess.claim(
                group: ensured.group, identity: ensured.identity, accountIdB64: accountIdB64, ds: ds)
            try DeviceGroupDsAccess.requireFoundingClaim(group: ensured.group)
            let proof = try ensured.group.dsAccessSigner(identity: ensured.identity)
            guard let built = try build(ensured) else { return nil }
            let next = try ensured.group.dsAccessSigner(identity: ensured.identity)
            let entry = DeviceGroupCommitEntry(
                kind: built.kind,
                journalIdB64: base64Encode(try ensured.group.groupId()),
                epoch: next.epoch(),
                epochKeyB64: base64Encode(next.verifyingKey()),
                rowB64: base64Encode(built.row),
                welcomeB64: built.welcome.map { base64Encode($0) },
                welcomeRendezvousB64: built.welcomeRendezvous.map { base64Encode($0) },
                nextSnapshotB64: base64Encode(try ensured.identity.snapshotStorage()),
                createdAtMs: Int64(Date().timeIntervalSince1970 * 1000))
            let outcome = try await DeviceGroupCommitOutbox.run(entry, proof: proof, ports: ports)
            guard case .landed = outcome else { return (outcome, nil) }
            return (outcome, try await after(ensured))
        }
    }

    /// Refused: another commit took the DeviceGroup's epoch first. The person
    /// tries again (the device applied the winner's commit meanwhile).
    public struct Lost: Error, Equatable {}
}
