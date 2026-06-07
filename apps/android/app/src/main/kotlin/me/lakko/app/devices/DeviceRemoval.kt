// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.ds.DsClient

/**
 * Remove-device flow on Android (Phase 2a3c).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/remove.ts`
 *   * `apps/ios/Lakko/Devices/DeviceRemoval.swift`
 *
 * Two call sites the Compose view exercises:
 *
 *   * [listDevices] — enumerate every device leaf in the
 *     user's DeviceGroup + tell the UI which row is "this
 *     device" so its Remove button can be disabled. For the
 *     self case the user uses `forgetDevice` on THIS device
 *     instead of Remove on another device.
 *
 *   * [removeDeviceAt] — build the MLS Remove commit for the
 *     given leaf, broadcast it to every remaining device via
 *     the DS group journal so they advance past the revoked
 *     leaf and ratchet group secrets out of the removed
 *     device's reach, persist the post-remove snapshot.
 *
 * No new server endpoints — the existing anonymous
 * `POST /ds/group/{group_id_hex}/messages` route is exactly
 * the journal we publish the commit to. `group_id` is the
 * deterministic
 * `BLAKE3("lakko-device-group/v1" || account_id)` value the
 * server treats as opaque bytes.
 */
public object DeviceRemoval {

    /** One row of the device list. */
    public data class DeviceListItem(
        val leafIndex: UInt,
        val signaturePublicKey: ByteArray,
        val isSelf: Boolean,
    ) {
        // Override equals / hashCode for the `signaturePublicKey`
        // ByteArray — data-class default uses reference
        // equality on arrays, which the Compose key inference
        // doesn't like.
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is DeviceListItem) return false
            return leafIndex == other.leafIndex &&
                signaturePublicKey.contentEquals(other.signaturePublicKey) &&
                isSelf == other.isSelf
        }

        override fun hashCode(): Int {
            var result = leafIndex.hashCode()
            result = 31 * result + signaturePublicKey.contentHashCode()
            result = 31 * result + isSelf.hashCode()
            return result
        }
    }

    /** Result of [listDevices]. */
    public data class DeviceList(
        val ownLeafIndex: UInt,
        val devices: List<DeviceListItem>,
    )

    /**
     * Snapshot the current DeviceGroup membership for
     * rendering the management screen.
     *
     * Builds a fresh DeviceIdentity + ensures the local
     * DeviceGroup, then closes the FFI handles before
     * returning. The list is a snapshot — the UI calls this
     * again after every mutation (add or remove) to pick up
     * the new epoch's leaf numbering.
     *
     * Sync — no DS round-trip. Safe to call from a Compose
     * `LaunchedEffect` body without `withContext(IO)` because
     * the work is FFI-only + sub-millisecond on real devices.
     */
    public fun listDevices(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
    ): DeviceList {
        val seed = DeviceIdentityStore.ensureDeviceIdentitySeed(
            context = context,
            accountIdB64 = accountIdB64,
        )
        val ensured = DeviceGroupLifecycle.ensureDeviceGroup(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            deviceIdentitySeed = seed,
        )
        seed.fill(0)
        try {
            val own = ensured.group.ownLeafIndex()
            val items = ensured.group.devices().map { row ->
                DeviceListItem(
                    leafIndex = row.leafIndex,
                    signaturePublicKey = row.signaturePublicKey,
                    isSelf = row.leafIndex == own,
                )
            }
            return DeviceList(ownLeafIndex = own, devices = items)
        } finally {
            try {
                ensured.group.close()
            } catch (_: Throwable) {
                // best-effort
            }
            try {
                ensured.identity.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    /**
     * Revoke `leafIndex` from the DeviceGroup.
     *
     * Steps:
     *
     *   1. Ensure the local DeviceGroup (load existing
     *      snapshot or bootstrap a founder if absent — the
     *      latter shouldn't happen on this path).
     *   2. Build the Remove commit. The local view advances
     *      atomically inside `removeDeviceAt`.
     *   3. Broadcast the commit to the DeviceGroup DS journal
     *      so every other still-paired device advances to the
     *      new epoch on its next poll.
     *   4. Persist the resulting snapshot.
     *
     * Throws inner errors verbatim:
     *
     *   * `FfiException.Mls("…cannot remove own leaf…")` when
     *     `leafIndex == ownLeafIndex()` (UI guards against
     *     this, but the Rust side rejects defensively).
     *   * `FfiException.Mls("…no such device leaf…")` for
     *     stale indices (a concurrent remove landed between
     *     `listDevices` and this call). UI re-fetches.
     *   * `DsHttpError` / `DsNetworkError` surface network /
     *     server failures on the commit broadcast.
     */
    public suspend fun removeDeviceAt(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        leafIndex: UInt,
        // G-13 (ADR-0015): the removal attestation from `/auth/action/finish`,
        // carried in the journal envelope so peers can verify the removal when
        // the DeviceGroup policy requires it. `null` for OPAQUE-only accounts.
        attestation: ByteArray? = null,
    ) {
        val seed = DeviceIdentityStore.ensureDeviceIdentitySeed(
            context = context,
            accountIdB64 = accountIdB64,
        )
        val ensured = DeviceGroupLifecycle.ensureDeviceGroup(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            deviceIdentitySeed = seed,
        )
        seed.fill(0)

        try {
            val commitBytes = ensured.group.removeDeviceAt(
                identity = ensured.identity,
                leafIndex = leafIndex,
            )
            val groupId = ensured.group.groupId()

            // Broadcast to the DeviceGroup journal. The DS treats the
            // deterministic 32-byte group_id as opaque bytes. G-13
            // (ADR-0015): wrap the commit + removal attestation in the journal
            // envelope so peers can verify the removal under the policy.
            val envelope = DeviceGroupJournalEnvelope.encode(commitBytes, attestation)
            DsClient().use { ds ->
                ds.publishGroup(groupId = groupId, payload = envelope)
            }

            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = ensured.identity,
                accountIdB64 = accountIdB64,
            )
        } finally {
            try {
                ensured.group.close()
            } catch (_: Throwable) {
                // best-effort
            }
            try {
                ensured.identity.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}
