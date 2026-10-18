// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
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
     *   3. Persist the resulting snapshot and reopen the sync
     *      runtime's handle from it, before anything is published (G-7).
     *   4. Broadcast the commit to the DeviceGroup DS journal
     *      so every other still-paired device advances to the
     *      new epoch on its next poll.
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
        /** ADR-0081 decision 4 — the removal of a rejoining device's previous
         *  leaf (diagnostics only). */
        rejoin: Boolean = false,
    ) {
        // ADR-0081 decision 2 — through the DeviceGroup commit outbox: the
        // entry (the ADR-0015 envelope around the Remove, with the
        // attestation) beside the N snapshot, the commit and the N+1
        // registration in one request, then N+1 made current and the sync
        // runtime reopened. Nothing that reads the snapshot can see N+1 before
        // the DS has the commit.
        val result = DeviceGroupCommits.commitChange(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            build = { ensured ->
                val commitBytes = ensured.group.removeDeviceAt(
                    identity = ensured.identity,
                    leafIndex = leafIndex,
                )
                DeviceGroupCommits.Built(
                    kind = if (rejoin) DeviceGroupCommitEntry.Kind.REJOIN_REMOVE else DeviceGroupCommitEntry.Kind.REMOVE,
                    row = DeviceGroupJournalEnvelope.encode(commitBytes, attestation),
                )
            },
            after = { },
        )
        if (result?.outcome is DeviceGroupCommitOutcome.Lost) throw DeviceGroupCommits.Lost()
    }

    /**
     * ADR-0074 section 2.3 step 4 — the DeviceGroup Remove for the device
     * whose leaf signature key is [deviceKey], resolved against the roster
     * as it is NOW (an owed removal is retried at a later launch, when leaf
     * indices may have moved).
     *
     *   * no leaf holds the key -> [GroupRemoval.NotInRoster] (already out);
     *   * the account's policy requires an attested removal (ADR-0015) and
     *     [attestation] is `null` -> [GroupRemoval.NeedsAttestation], and
     *     nothing is committed: every peer would refuse the commit and this
     *     device would be left an epoch ahead of them;
     *   * otherwise [removeDeviceAt], whose errors propagate.
     */
    public suspend fun removeDeviceByKey(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        deviceKey: ByteArray,
        attestation: ByteArray?,
    ): GroupRemoval {
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
        val (leaf, attestedOnly) = try {
            val own = ensured.group.ownLeafIndex()
            val row = ensured.group.devices().firstOrNull {
                it.leafIndex != own && it.signaturePublicKey.contentEquals(deviceKey)
            }
            row?.leafIndex to ensured.group.requireAttestedRemovals()
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
        if (leaf == null) return GroupRemoval.NotInRoster
        if (attestedOnly && attestation == null) return GroupRemoval.NeedsAttestation
        removeDeviceAt(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            leafIndex = leaf,
            attestation = attestation,
        )
        return GroupRemoval.Removed
    }

    /**
     * G-13 (ADR-0015): enable the attested-removal policy on the local
     * DeviceGroup if it is not already on, broadcasting the
     * GroupContextExtensions commit over the journal so every device enforces
     * it on subsequent removals. Called after a passkey is first enabled.
     * Idempotent — a no-op when the policy is already set.
     */
    public suspend fun enableAttestedRemovalsIfNeeded(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
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
        val alreadyOn = try {
            ensured.group.requireAttestedRemovals()
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
        if (alreadyOn) return
        // ADR-0081 decision 2 — through the DeviceGroup commit outbox.
        val result = DeviceGroupCommits.commitChange(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            build = { fresh ->
                // Another device enabled it meanwhile.
                if (fresh.group.requireAttestedRemovals()) {
                    null
                } else {
                    // The policy commit removes nothing, so it carries no attestation.
                    DeviceGroupCommits.Built(
                        kind = DeviceGroupCommitEntry.Kind.POLICY,
                        row = DeviceGroupJournalEnvelope.encode(
                            fresh.group.setRequireAttestedRemovals(fresh.identity), null,
                        ),
                    )
                }
            },
            after = { },
        )
        if (result?.outcome is DeviceGroupCommitOutcome.Lost) throw DeviceGroupCommits.Lost()
    }
}
