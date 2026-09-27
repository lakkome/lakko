// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0053 — the DeviceGroup journal is claimed like a conversation group's:
/// its access key is derived per epoch from the DeviceGroup itself
/// (`DeviceGroup.dsAccessSigner`), every read and write carries a proof, the
/// journal is claimed at founding and on every open, and every commit
/// registers the next epoch before it is published.
///
/// A DeviceGroup handle is rebuilt from the snapshot by each flow that needs
/// one (``DeviceGroupLifecycle/ensureDeviceGroup(accountIdBytes:accountIdB64:deviceIdentitySeed:snapshotStore:)``),
/// so there is no long-lived previous-epoch signer to fall back on here: a
/// refused request is retried once after a claim-on-open.
public enum DeviceGroupDsAccess {

    /// The auth for a request to `group`'s journal, or `nil` when the signer
    /// cannot be derived (the request then goes unsigned, which only an
    /// unclaimed journal accepts).
    public static func auth(group: DeviceGroup, identity: Identity, ds: DsClient) -> DsAuth? {
        guard let signer = try? group.dsAccessSigner(identity: identity) else { return nil }
        return DsAuth(
            signer: signer,
            reclaim: { await claim(group: group, identity: identity, ds: ds) }
        )
    }

    /// Claim-on-open (contract point 2): register the current epoch's key.
    /// Idempotent; unsigned (a first claim, or a re-send of a registered key).
    /// A failure is logged and retried on the next open. Returns the signer.
    @discardableResult
    public static func claim(group: DeviceGroup, identity: Identity, ds: DsClient) async -> DsRequestSigner? {
        guard let signer = try? group.dsAccessSigner(identity: identity),
              let groupId = try? group.groupId() else { return nil }
        do {
            try await ds.claimGroup(
                groupId: groupId, bodyJson: signer.groupClaimBodyJson(), provenBy: nil)
        } catch {
            NSLog("ADR-0053: DeviceGroup journal claim failed, retried on the next open: %@",
                  String(describing: error))
        }
        return signer
    }

    /// Register-before-publish (contract point 3) for one direct DeviceGroup
    /// commit. `create` eager-merges N -> N+1 on a throwaway handle; the N+1
    /// key is registered proven under N; `prepare` runs next (whatever must be
    /// durable before the publish); `publish` goes out signed under N. A
    /// failed registration publishes nothing and leaves the on-disk snapshot at
    /// N — the handle itself is discarded by the caller, so there is nothing
    /// in memory to roll back.
    public static func directCommit<Created, Prepared, Published>(
        group: DeviceGroup,
        identity: Identity,
        ds: DsClient,
        create: @escaping () throws -> Created,
        prepare: @escaping (Created) throws -> Prepared,
        publish: @escaping (Prepared, DsAuth) async throws -> Published
    ) async throws -> Published {
        let groupId = try group.groupId()
        return try await DirectCommitSteps(
            currentSigner: { try group.dsAccessSigner(identity: identity) },
            create: create,
            register: { next, provenBy in
                try await ds.claimGroup(
                    groupId: groupId, bodyJson: next.groupClaimBodyJson(), provenBy: provenBy)
            },
            prepare: prepare,
            rollback: {},
            publish: { prepared, signerN in
                try await publish(prepared, DsAuth(signer: signerN))
            }
        ).run()
    }

    /// Heal on apply (contract point 4): a batch that moved the epoch
    /// registers the new key proven under the pre-batch signer. Best effort.
    public static func heal(
        groupId: Data,
        before: DsGroupSigner?,
        after: DsGroupSigner?,
        ds: DsClient
    ) async {
        await dsHealRegistration(ds: ds, groupId: groupId, before: before, after: after)
    }
}
