// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.registerBeforePublish
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0053 — the DeviceGroup journal is claimed like any group journal, under
 * the per-epoch access key `DeviceGroup.dsAccessSigner` derives.
 *
 * The DeviceGroup's commit paths ([PairingOrchestrator], [DeviceRemoval]) each
 * open their own short-lived handle, so they take the signers from that handle
 * explicitly: [signerOf] before the commit (epoch N) and after it (N+1), then
 * [registerNext] before anything is persisted or published.
 */
internal object DeviceGroupJournalAccess {
    /** The [me.lakko.app.ds.DsSignerRegistry] owner tag of the sync
     *  repository's installed source. */
    const val REGISTRY_OWNER: String = "devicegroup"

    /** [group]'s journal signer at its current epoch, or `null` when the
     *  handle cannot derive one. */
    fun signerOf(group: DeviceGroup, identity: Identity): DsJournalSigner? =
        try {
            FfiDsJournalSigner(group.dsAccessSigner(identity))
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0053 client contract point 3 — register epoch N+1's key ([after])
     * proven under N's ([before]) and return [before], the signer the commit
     * POST is signed with. Throws when the registration cannot be made or is
     * refused; the caller must then publish nothing.
     */
    suspend fun registerNext(
        ds: DsClient,
        groupId: ByteArray,
        before: DsJournalSigner?,
        after: DsJournalSigner?,
    ): DsJournalSigner =
        registerBeforePublish(before, after) { next, current ->
            ds.registerNextEpoch(groupId, next, current)
        }
}
