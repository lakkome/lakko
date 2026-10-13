// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Remove-device flow on iOS (Phase 2a3c).
///
/// Mirror of `apps/web/src/devices/remove.ts` and Android's
/// `me.lakko.app.devices.DeviceRemoval`. Two call sites the
/// SwiftUI view exercises:
///
///   * [`listDevices`] — enumerate every device leaf in the
///     user's DeviceGroup + tell the UI which row is "this
///     device" so its Remove button can be disabled. For the
///     self case the user uses `forgetDevice` on THIS device
///     instead of Remove from another device.
///
///   * [`removeDeviceAt`] — build the MLS Remove commit for
///     the given leaf, broadcast it to every remaining device
///     via the DS group journal so they advance past the
///     revoked leaf and ratchet group secrets out of the
///     removed device's reach, persist the post-remove
///     snapshot.
///
/// No new server endpoints — the existing anonymous
/// `POST /ds/group/{group_id_hex}/messages` route is exactly
/// the journal we publish the commit to. `group_id` is the
/// deterministic
/// `BLAKE3("lakko-device-group/v1" || account_id)` value the
/// server treats as opaque.
public enum DeviceRemoval {

    /// One row of the device list.
    public struct DeviceListItem: Identifiable, Equatable {
        public let leafIndex: UInt32
        public let signaturePublicKey: Data
        public let isSelf: Bool

        public var id: UInt32 { leafIndex }

        public init(leafIndex: UInt32, signaturePublicKey: Data, isSelf: Bool) {
            self.leafIndex = leafIndex
            self.signaturePublicKey = signaturePublicKey
            self.isSelf = isSelf
        }
    }

    /// Result of [`listDevices`].
    public struct DeviceList: Equatable {
        public let ownLeafIndex: UInt32
        public let devices: [DeviceListItem]

        public init(ownLeafIndex: UInt32, devices: [DeviceListItem]) {
            self.ownLeafIndex = ownLeafIndex
            self.devices = devices
        }
    }

    /// Snapshot the current DeviceGroup membership for
    /// rendering the management screen.
    ///
    /// Builds a fresh DeviceIdentity + ensures the local
    /// DeviceGroup, then drops the FFI handles before
    /// returning. The list is a snapshot — the UI calls this
    /// again after every mutation (add or remove) to pick up
    /// the new epoch's leaf numbering.
    public static func listDevices(
        accountIdBytes: Data,
        accountIdB64: String
    ) throws -> DeviceList {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )
        let ownLeafIndex = try ensured.group.ownLeafIndex()
        let rows = try ensured.group.devices()
        let items = rows.map { row in
            DeviceListItem(
                leafIndex: row.leafIndex,
                signaturePublicKey: row.signaturePublicKey,
                isSelf: row.leafIndex == ownLeafIndex
            )
        }
        return DeviceList(ownLeafIndex: ownLeafIndex, devices: items)
    }

    /// Revoke `leafIndex` from the DeviceGroup.
    ///
    /// Steps:
    ///
    ///   1. Ensure the local DeviceGroup (load existing
    ///      snapshot or bootstrap a founder if absent — the
    ///      latter shouldn't happen on this path).
    ///   2. Build the Remove commit. The local view advances
    ///      atomically inside `removeDeviceAt`.
    ///   3. Persist the resulting snapshot, before anything is
    ///      published (G-7).
    ///   4. Broadcast the commit to the DeviceGroup DS journal
    ///      so every other still-paired device advances to the
    ///      new epoch on its next poll.
    ///
    /// Throws inner errors verbatim:
    ///
    ///   * `FfiError.mls("…cannot remove own leaf…")` when
    ///     `leafIndex == ownLeafIndex()` (UI guards against
    ///     this, but the Rust side rejects defensively).
    ///   * `FfiError.mls("…no such device leaf…")` for stale
    ///     indices (a concurrent remove landed between
    ///     `listDevices` and this call). UI re-fetches.
    ///   * `DsClientError` surfaces network / server failures
    ///     on the commit broadcast.
    public static func removeDeviceAt(
        accountIdBytes: Data,
        accountIdB64: String,
        leafIndex: UInt32,
        // G-13 (ADR-0015): the removal attestation from `/auth/action/finish`,
        // carried in the journal envelope so peers can verify the removal when
        // the DeviceGroup policy requires it. `nil` for OPAQUE-only accounts.
        attestation: Data? = nil
    ) async throws {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )

        let groupId = try ensured.group.groupId()
        let ds = DsClient()
        await DeviceGroupDsAccess.claim(
            group: ensured.group, identity: ensured.identity, accountIdB64: accountIdB64, ds: ds)

        // ADR-0053 point 3 — register N+1 proven under N before the publish.
        // Broadcast to the DeviceGroup journal. The DS treats the
        // deterministic 32-byte group_id as opaque bytes. G-13 (ADR-0015):
        // wrap the commit + removal attestation in the journal envelope so
        // peers can verify the removal under the policy.
        _ = try await DeviceGroupDsAccess.directCommit(
            group: ensured.group,
            identity: ensured.identity,
            accountIdB64: accountIdB64,
            ds: ds,
            create: {
                try ensured.group.removeDeviceAt(
                    identity: ensured.identity,
                    leafIndex: leafIndex
                )
            },
            // G-7 — the advanced state is on disk before the commit is
            // published, so nothing that reads the snapshot (the sync
            // runtime's reload, the next add) can see the pre-removal epoch.
            prepare: { commitBytes in
                DeviceGroupLifecycle.persistSnapshot(
                    identity: ensured.identity,
                    accountIdB64: accountIdB64
                )
                return commitBytes
            },
            publish: { commitBytes, auth in
                let envelope = DeviceGroupJournalEnvelope.encode(
                    message: commitBytes,
                    attestation: attestation
                )
                return try await ds.publishGroup(groupId: groupId, payload: envelope, auth: auth)
            }
        )
    }

    /// ADR-0074 section 2.3 step 4 — remove the leaf holding `deviceKey` from
    /// the DeviceGroup, found by KEY (a leaf index does not survive a
    /// relaunch, and the owed record of a removal names the device).
    ///
    /// `.done` when the commit was published or no leaf holds the key (an
    /// earlier run, or another device, already removed it). When the
    /// attested-removal policy is on and no attestation is in hand, nothing
    /// is committed and `.needsAttestation` is returned: peers refuse an
    /// unattested Remove under the policy, so committing one would fork this
    /// device from them.
    public static func removeDevice(
        accountIdBytes: Data,
        accountIdB64: String,
        deviceKey: Data,
        attestation: Data?
    ) async throws -> DeviceRemovalPlan.DeviceGroupOutcome {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )
        guard let leaf = try ensured.group.devices()
            .first(where: { $0.signaturePublicKey == deviceKey })?.leafIndex else {
            return .done
        }
        if attestation == nil, try ensured.group.requireAttestedRemovals() {
            return .needsAttestation
        }
        try await removeDeviceAt(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            leafIndex: leaf,
            attestation: attestation
        )
        return .done
    }

    /// The leaf keys of the account's DeviceGroup as the persisted snapshot
    /// has them, or `nil` when it cannot be read.
    public static func rosterKeys(accountIdBytes: Data, accountIdB64: String) -> [Data]? {
        (try? listDevices(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64))?
            .devices.map(\.signaturePublicKey)
    }

    /// G-13 (ADR-0015): enable the attested-removal policy on the local
    /// DeviceGroup if it is not already on, broadcasting the
    /// GroupContextExtensions commit over the journal so every device enforces
    /// it on subsequent removals. Called after a passkey is first enabled.
    /// Idempotent — a no-op when the policy is already set.
    public static func enableAttestedRemovalsIfNeeded(
        accountIdBytes: Data,
        accountIdB64: String
    ) async throws {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )
        if try ensured.group.requireAttestedRemovals() {
            return
        }
        let groupId = try ensured.group.groupId()
        let ds = DsClient()
        await DeviceGroupDsAccess.claim(
            group: ensured.group, identity: ensured.identity, accountIdB64: accountIdB64, ds: ds)
        // ADR-0053 point 3 — register N+1 proven under N before the publish.
        _ = try await DeviceGroupDsAccess.directCommit(
            group: ensured.group,
            identity: ensured.identity,
            accountIdB64: accountIdB64,
            ds: ds,
            create: { try ensured.group.setRequireAttestedRemovals(identity: ensured.identity) },
            // G-7 — on disk before the publish (see `removeDeviceAt`).
            prepare: { commit in
                DeviceGroupLifecycle.persistSnapshot(
                    identity: ensured.identity,
                    accountIdB64: accountIdB64
                )
                return commit
            },
            publish: { commit, auth in
                // The policy commit removes nothing, so it carries no attestation.
                let envelope = DeviceGroupJournalEnvelope.encode(message: commit, attestation: nil)
                return try await ds.publishGroup(groupId: groupId, payload: envelope, auth: auth)
            }
        )
    }
}
