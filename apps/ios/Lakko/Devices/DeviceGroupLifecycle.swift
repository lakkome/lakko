// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// DeviceGroup lifecycle on iOS (Phase 2a3b3, ADR-0007).
///
/// Mirror of `apps/web/src/devices/lifecycle.ts`. One entry
/// point — [`ensureDeviceGroup`] — wraps the "load existing or
/// create-founder" decision plus the post-mutation snapshot
/// persistence so the per-screen call sites see a single
/// function.
///
/// The DeviceIdentity wasm/UniFFI handle this module builds
/// from the cached `deviceIdentitySeed` has its own in-memory
/// OpenMLS provider, independent of the AccountIdentity
/// provider that ``ChatRepository`` holds for conversation
/// groups (ADR-0007). Snapshot/restore goes through this
/// DeviceIdentity provider's storage only.
public enum DeviceGroupLifecycle {

    /// Result of [`ensureDeviceGroup`].
    public struct Ensured {
        /// The Identity built from the DeviceIdentity seed,
        /// populated with the (loaded or freshly-bootstrapped)
        /// MLS provider state. Caller holds this for the
        /// duration of the operation.
        public let identity: Identity
        /// The live DeviceGroup handle.
        public let group: DeviceGroup
        /// `true` when this call bootstrapped a fresh
        /// DeviceGroup (this device is now the founder).
        public let bootstrapped: Bool
    }

    /// Resolve the local DeviceGroup state: either restore
    /// the cached snapshot or bootstrap a fresh founder leaf.
    ///
    /// Idempotent on the snapshot side — calling twice with
    /// the same `(accountIdB64, deviceIdentitySeed)` returns
    /// the same group (the bootstrap branch fires only when
    /// the snapshot is absent).
    public static func ensureDeviceGroup(
        accountIdBytes: Data,
        accountIdB64: String,
        deviceIdentitySeed: Data,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws -> Ensured {
        // Build the per-device Identity from the cached seed.
        // This is the DeviceIdentity (ADR-0007), NOT the
        // AccountIdentity ChatRepository holds.
        let identity = try Identity.fromSeed(seed: deviceIdentitySeed)

        // Try to restore prior MLS state.
        if let snapshot = snapshotStore.getSnapshot(accountIdB64: accountIdB64) {
            do {
                try identity.restoreStorage(snapshot: snapshot)
                if let group = try loadDeviceGroup(
                    identity: identity,
                    accountId: accountIdBytes
                ) {
                    return Ensured(
                        identity: identity,
                        group: group,
                        bootstrapped: false
                    )
                }
                // Snapshot existed but didn't carry the
                // DeviceGroup row — fall through to
                // bootstrap. Defensive: shouldn't happen in
                // normal flows because every snapshot we
                // write was captured immediately after a
                // DeviceGroup mutation.
            } catch {
                NSLog("DeviceGroup snapshot restore failed: \(error); bootstrapping")
                // Fall through.
            }
        }

        // Bootstrap. Local device becomes the founding leaf.
        let group = try DeviceGroup.createFounder(
            identity: identity,
            accountId: accountIdBytes
        )
        let fresh = try identity.snapshotStorage()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: fresh)
        return Ensured(identity: identity, group: group, bootstrapped: true)
    }

    /// Re-capture the MLS provider snapshot after a
    /// DeviceGroup mutation (add-device, remove-device,
    /// application-message send). Best-effort — a snapshot
    /// persist failure logs and continues.
    public static func persistSnapshot(
        identity: Identity,
        accountIdB64: String,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) {
        do {
            let snapshot = try identity.snapshotStorage()
            snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshot)
        } catch {
            NSLog("DeviceGroup snapshot persist failed: \(error)")
        }
    }
}
