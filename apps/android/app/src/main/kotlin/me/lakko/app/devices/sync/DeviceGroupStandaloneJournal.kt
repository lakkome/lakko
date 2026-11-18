// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import android.util.Log
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.auth.JournalCatchUp
import me.lakko.app.auth.StandaloneJournal
import me.lakko.app.auth.StandaloneJournalPage
import me.lakko.app.auth.StandaloneJournalRow
import me.lakko.app.devices.DeviceGroupJournalAccess
import me.lakko.app.devices.DeviceGroupJournalEnvelope
import me.lakko.app.devices.DeviceGroupLifecycle
import me.lakko.app.devices.DeviceGroupSnapshotStore
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.DsProofRefusedError
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.DeviceGroupBody
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope
import uniffi.lakko_ffi_uniffi.loadDeviceGroup
import uniffi.lakko_ffi_uniffi.parseDevicegroupPayload

/**
 * ADR-0074 section 3.3 - this device's DeviceGroup opened from its own disk
 * for one read of the journal, outside any session ([me.lakko.app.auth.readJournalStandalone]).
 *
 * Read-only: the snapshot is restored into a throwaway [Identity] and never
 * written back, the cursor is read and never moved, an unusable snapshot is
 * left where it is (the sync runtime decides about it), and nothing is claimed
 * on the DS. Requests are signed with the DeviceGroup's own epoch key, never a
 * session token.
 */
internal class DeviceGroupStandaloneJournal private constructor(
    private val identity: Identity,
    private val group: DeviceGroup,
    private val groupId: ByteArray,
    override val cursor: Long,
    /** The device-attestation verifying key the removal check needs. */
    private val attestVk: ByteArray,
) : StandaloneJournal {

    private val ds = DsClient()

    override suspend fun fetch(after: Long): StandaloneJournalPage {
        val signer = DeviceGroupJournalAccess.signerOf(group, identity)
            ?: throw IllegalStateException("no DeviceGroup access key")
        val result = ds.fetchGroup(
            groupId = groupId,
            after = after,
            limit = null,
            waitSecs = 0,
            signer = signer,
            reclaimOnRefusal = false,
        )
        return StandaloneJournalPage(
            rows = result.messages.map { StandaloneJournalRow(it.id, it.payload) },
            hasMore = result.hasMore,
        )
    }

    override fun apply(row: StandaloneJournalRow): ByteArray? {
        val entry = DeviceGroupJournalEnvelope.decode(row.payload)
        val processed = group.processIncomingVerifyingRemovals(
            identity,
            entry.first,
            entry.second?.let { listOf(it) } ?: emptyList(),
            attestVk,
            System.currentTimeMillis() / 1000,
        )
        if (processed !is ProcessedMessage.Application) return null
        val body = parseDevicegroupPayload(processed.`data`).body
        if (body !is DeviceGroupBody.ServiceEnvelope) return null
        val isRotation = try {
            dispatchIncomingEnvelope(body.envelope).kind == FfiIncomingEnvelopeKind.KEY_ROTATION
        } catch (_: Throwable) {
            false
        }
        return if (isRotation) body.envelope else null
    }

    override fun close() {
        ds.close()
        group.close()
        identity.close()
    }

    companion object {
        private const val LOG_TAG = "DeviceGroupSync"

        /**
         * Open this device's persisted DeviceGroup, or `null` when it holds no
         * usable state (no device seed, no account seed, no snapshot, or one
         * that does not restore). Throws when the attestation key cannot be
         * had (no answer: the read is tried again later).
         */
        suspend fun open(context: Context, accountIdBytes: ByteArray, accountIdB64: String): StandaloneJournal? {
            val deviceSeed = DeviceIdentityStore.getDeviceIdentitySeed(context, accountIdB64) ?: return null
            val lineage = DeviceGroupLifecycle.lineageSeed(context, accountIdB64)
            if (lineage == null) {
                deviceSeed.fill(0)
                return null
            }
            val snapshot = try {
                DeviceGroupSnapshotStore.getSnapshot(context, accountIdB64)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "standalone read: DeviceGroup snapshot not readable (${cause.javaClass.simpleName})")
                null
            }
            if (snapshot == null) {
                deviceSeed.fill(0)
                lineage.fill(0)
                return null
            }
            val identity = try {
                Identity.fromSeed(deviceSeed)
            } finally {
                deviceSeed.fill(0)
            }
            val group = try {
                Identity.fromSeed(lineage).use { lineageAccount ->
                    identity.restoreStorage(snapshot)
                    loadDeviceGroup(identity, accountIdBytes, lineageAccount)
                }
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "standalone read: DeviceGroup snapshot does not restore (${cause.javaClass.simpleName})")
                identity.close()
                null
            } finally {
                lineage.fill(0)
            } ?: return null
            try {
                val groupId = group.groupId()
                val cursor = runCatching { DeviceGroupJournalCursor.read(context, accountIdB64, groupId) }.getOrDefault(0L)
                val attestVk = me.lakko.app.devices.DeviceAttestKey.fetch()
                return DeviceGroupStandaloneJournal(identity, group, groupId, cursor, attestVk)
            } catch (cause: Throwable) {
                group.close()
                identity.close()
                throw cause
            }
        }

        /** What a failed fetch means for the decision. */
        fun classify(cause: Throwable): JournalCatchUp = when {
            cause is DsProofRefusedError && cause.clockSkewed -> JournalCatchUp.Offline
            cause is DsHttpError && (cause.status == 401 || cause.status == 403 || cause.status == 404) ->
                JournalCatchUp.Unreadable
            cause is DsNetworkError -> JournalCatchUp.Offline
            else -> JournalCatchUp.Offline
        }
    }
}
