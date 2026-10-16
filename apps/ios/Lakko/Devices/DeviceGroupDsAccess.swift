// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0053 + ADR-0057 — the DeviceGroup journal is opened like a conversation
/// group's: its access key is derived per epoch from the DeviceGroup itself
/// (`DeviceGroup.dsAccessSigner`) and every read and write carries a proof.
///
/// ADR-0057: the DeviceGroup's OWNER is the account, so every device of the
/// account holds the journal ROOT (`dsJournalRootSigner` over the ACCOUNT
/// identity and the DeviceGroup id) and registers every epoch with it. There
/// are no registrars. A DeviceGroup founded here (``DeviceGroupLifecycle``
/// marks it) claims with the root and, when the id was claimed before by the
/// same root (a re-found after lost local state), replaces the chain with a
/// `reset` (obligation 7). A device that is merely behind never resets.
///
/// A DeviceGroup handle is rebuilt from the snapshot by each flow that needs
/// one, so there is no long-lived previous-epoch signer to fall back on here:
/// a refused request is retried once after the root's lazy claim.
public enum DeviceGroupDsAccess {

    /// The journal root of the account's DeviceGroup, or `nil` when the
    /// account identity seed is not on this device (a fresh device before its
    /// phrase), in which case nothing can be registered.
    static func rootSigner(accountIdB64: String, groupId: Data) -> DsStatementSignerProtocol? {
        guard let seed = try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64),
              let account = try? Identity.fromSeed(seed: seed) else { return nil }
        return try? dsJournalRootSigner(accountIdentity: account, journalId: groupId)
    }

    /// The auth for a request to `group`'s journal, or `nil` when the signer
    /// cannot be derived (the request then goes unsigned, which only an
    /// unclaimed journal accepts).
    public static func auth(
        group: DeviceGroup,
        identity: Identity,
        accountIdB64: String,
        ds: DsClient
    ) -> DsAuth? {
        guard let signer = try? group.dsAccessSigner(identity: identity) else { return nil }
        return DsAuth(
            signer: signer,
            reclaim: { await lazyClaim(group: group, identity: identity, accountIdB64: accountIdB64, ds: ds) }
        )
    }

    /// Claim on open. For a DeviceGroup this device just FOUNDED, claim with
    /// the root and reset on a `409`; otherwise claim (idempotent when the key
    /// is already registered at that epoch) and leave a `409` alone — a device
    /// that is behind must not rewind the chain. Best effort, logged.
    @discardableResult
    public static func claim(
        group: DeviceGroup,
        identity: Identity,
        accountIdB64: String,
        ds: DsStatementSending,
        marks: JournalFoundingMarks = .shared
    ) async -> DsRequestSigner? {
        guard let signer = try? group.dsAccessSigner(identity: identity),
              let groupId = try? group.groupId(),
              let root = rootSigner(accountIdB64: accountIdB64, groupId: groupId) else { return nil }
        let hex = DsClient.pseudonymHex(groupId)
        do {
            if marks.isMarked(journalIdHex: hex) {
                try await JournalOwnership.claimOrReset(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
                marks.clear(journalIdHex: hex)
            } else {
                try await JournalOwnership.claim(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
            }
        } catch DsStatementError.conflict {
            // Claimed by this root at another chain: this device is behind or
            // ahead; the next commit registers under the root.
        } catch {
            NSLog("ADR-0057: DeviceGroup journal claim failed, retried on the next open: %@",
                  String(describing: error))
        }
        return signer
    }

    /// Second security pass, finding 3 — claims fail closed. A DeviceGroup
    /// founded here whose claim has not landed (its founding mark is still
    /// set, so ``claim(group:identity:accountIdB64:ds:marks:)`` did not get a
    /// 204) must not send any Welcome: this throws, and the caller publishes
    /// nothing.
    public static func requireFoundingClaim(
        group: DeviceGroup,
        marks: JournalFoundingMarks = .shared
    ) throws {
        let groupId = try group.groupId()
        if marks.isMarked(journalIdHex: DsClient.pseudonymHex(groupId)) {
            NSLog("ADR-0057: the founded DeviceGroup journal is not claimed; nothing is sent")
            throw DsStatementError.unauthorized
        }
    }

    /// The root's lazy claim after a `401` (obligation 2): every device of the
    /// account owns the DeviceGroup. An already-claimed journal (`409`) is left
    /// alone: nothing is re-registered under the root.
    static func lazyClaim(
        group: DeviceGroup,
        identity: Identity,
        accountIdB64: String,
        ds: DsStatementSending
    ) async -> DsRequestSigner? {
        guard let signer = try? group.dsAccessSigner(identity: identity),
              let groupId = try? group.groupId() else { return nil }
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: true,
            root: {
                guard let root = rootSigner(accountIdB64: accountIdB64, groupId: groupId) else {
                    throw DsStatementError.unauthorized
                }
                return root
            },
            journalId: groupId,
            epoch: signer.epoch(),
            epochKey: signer.verifyingKey(),
            ds: ds)
        switch outcome {
        case .claimed: return signer
        case .alreadyClaimed, .notOwner, .failed: return nil
        }
    }

    /// ADR-0073 section 5 — after a key reset, found the account's DeviceGroup
    /// afresh under the NEW device binding.
    ///
    /// ADR-0078 section 6: the DeviceGroup id is derived from the account
    /// seed, so the NEW seed names a NEW journal that nobody holds; the
    /// founding device claims it with the root its new seed derives (the
    /// ordinary founding claim, ``claim(group:identity:accountIdB64:ds:marks:)``).
    /// The attested reset of the OLD journal (and its root endorsement) is
    /// gone: the DS never sees a key-reset attestation. The old snapshot is
    /// dropped first (the old DeviceGroup is dead). The founding mark is
    /// cleared only once the claim landed, so a claim that did not land keeps
    /// the group off the wire (``requireFoundingClaim(group:marks:)``) and the
    /// next retry claims again.
    ///
    /// `accountSeed` is the NEW 64-byte account seed.
    public static func refoundAfterKeyReset(
        accountIdBytes: Data,
        accountIdB64: String,
        accountSeed: Data,
        ds: DsStatementSending,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore(),
        marks: JournalFoundingMarks = .shared
    ) async throws {
        guard let journalId = DeviceGroupLifecycle.groupId(
            accountIdB64: accountIdB64, currentSeed: accountSeed) else {
            throw DeviceGroupLifecycle.NoAccountSeed()
        }
        let hex = DsClient.pseudonymHex(journalId)
        // A snapshot of the OLD DeviceGroup, or of a re-founding whose claim
        // did not land: the latter is reused (its mark is still set), the
        // former dropped.
        if !marks.isMarked(journalIdHex: hex) {
            snapshotStore.deleteSnapshot(accountIdB64: accountIdB64)
        }
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed,
            accountSeed: accountSeed,
            snapshotStore: snapshotStore
        )
        guard try ensured.group.groupId() == journalId else {
            throw DsStatementError.malformed("the re-founded DeviceGroup has another journal id")
        }
        let account = try Identity.fromSeed(seed: accountSeed)
        let root = try dsJournalRootSigner(accountIdentity: account, journalId: journalId)
        let signer = try ensured.group.dsAccessSigner(identity: ensured.identity)
        try await JournalOwnership.claimOrReset(
            root: root,
            journalId: journalId,
            epoch: signer.epoch(),
            epochKey: signer.verifyingKey(),
            ds: ds)
        marks.clear(journalIdHex: hex)
    }

    /// ADR-0074 section 3.5, "DS" — after a ROTATION of the account keys,
    /// hand the DeviceGroup journal's root from the old seed's root to the
    /// new seed's: `PUT /ds/group/{hex}/root`, a `RootTransfer` signed by the
    /// OLD root naming the new root (the old root may always transfer). The
    /// DS keeps the chain, the registrars and the push binding (a kept device
    /// that lags an epoch is not cut off).
    ///
    /// The DeviceGroup is NOT re-founded and its id does not move: the id is
    /// derived from the seed the conversation leaves are bound to (ADR-0078
    /// section 6), the OLDEST own predecessor, which this device holds after
    /// the rotation. Since ADR-0078 phase 6 the transfer carries no
    /// key-reset attestation and no root endorsement.
    ///
    /// Idempotent on a retry: once the transfer landed the old root is a
    /// stranger (`401`); the root probe by the NEW root then confirms it
    /// (`204`), and anything else is thrown.
    public static func transferRootAfterKeyRotation(
        accountIdB64: String,
        predecessorSeed: Data,
        newAccountSeed: Data,
        ds: DsStatementSending
    ) async throws {
        guard let journalId = DeviceGroupLifecycle.groupId(
            accountIdB64: accountIdB64, currentSeed: newAccountSeed) else {
            throw DeviceGroupLifecycle.NoAccountSeed()
        }
        let oldRoot = try dsJournalRootSigner(
            accountIdentity: try Identity.fromSeed(seed: predecessorSeed), journalId: journalId)
        let newRoot = try dsJournalRootSigner(
            accountIdentity: try Identity.fromSeed(seed: newAccountSeed), journalId: journalId)
        let request = try oldRoot.rootTransfer(journalId: journalId, newRoot: newRoot.publicKey())
        do {
            try await ds.putStatement(request)
        } catch let refusal as DsStatementError where refusal == .conflict || refusal == .unauthorized {
            switch try await JournalOwnership.probeRoot(root: newRoot, journalId: journalId, ds: ds) {
            case .root:
                NSLog("ADR-0074: the DeviceGroup journal root transfer had already landed")
            case .notRoot:
                throw refusal
            }
        }
    }

    /// Register-before-publish (obligation 5) for one direct DeviceGroup
    /// commit. `create` eager-merges N -> N+1 on a throwaway handle; the N+1
    /// key is registered under the root; `prepare` runs next (whatever must be
    /// durable before the publish); `publish` goes out signed under N. A
    /// failed registration publishes nothing and leaves the on-disk snapshot at
    /// N — the handle itself is discarded by the caller, so there is nothing
    /// in memory to roll back.
    public static func directCommit<Created, Prepared, Published>(
        group: DeviceGroup,
        identity: Identity,
        accountIdB64: String,
        ds: DsClient,
        create: @escaping () throws -> Created,
        prepare: @escaping (Created) throws -> Prepared,
        publish: @escaping (Prepared, DsAuth) async throws -> Published
    ) async throws -> Published {
        let groupId = try group.groupId()
        return try await DirectCommitSteps(
            currentSigner: { try group.dsAccessSigner(identity: identity) },
            create: create,
            register: { next in
                guard let root = rootSigner(accountIdB64: accountIdB64, groupId: groupId) else {
                    throw DsStatementError.unauthorized
                }
                try await JournalOwnership.registerEpoch(
                    authority: .root(root), journalId: groupId, epoch: next.epoch(),
                    epochKey: next.verifyingKey(), revoke: [], ds: ds)
            },
            prepare: prepare,
            rollback: {},
            publish: { prepared, signerN in
                try await publish(prepared, DsAuth(signer: signerN))
            }
        ).run()
    }
}
