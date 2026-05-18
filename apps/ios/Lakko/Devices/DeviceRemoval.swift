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
    ///   3. Broadcast the commit to the DeviceGroup DS journal
    ///      so every other still-paired device advances to the
    ///      new epoch on its next poll.
    ///   4. Persist the resulting snapshot.
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
        leafIndex: UInt32
    ) async throws {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )

        let commitBytes = try ensured.group.removeDeviceAt(
            identity: ensured.identity,
            leafIndex: leafIndex
        )
        let groupId = try ensured.group.groupId()

        // Broadcast to the DeviceGroup journal. The DS treats
        // the deterministic 32-byte group_id as opaque bytes.
        let ds = DsClient()
        _ = try await ds.publishGroup(groupId: groupId, payload: commitBytes)

        DeviceGroupLifecycle.persistSnapshot(
            identity: ensured.identity,
            accountIdB64: accountIdB64
        )
    }
}
