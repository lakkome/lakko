// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Log
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/** How a `/g/` link is filed in the account's link register. */
public class LinkRegisterEntry(
    /** [LinkRecordKind.GROUP], [LinkRecordKind.CHANNEL] or
     *  [LinkRecordKind.PUBLISHER]. */
    public val kind: LinkRecordKind,
    /** The group id or the channel id the link opens. */
    public val targetId: ByteArray,
    /** A private channel subscriber link's invite seed, else empty. SECRET. */
    public val inviteSeed: ByteArray = ByteArray(0),
    /** The admission-key version a private channel link names, else 0. */
    public val admissionVersion: UShort = 0u,
    /** The owner's label for the link, or empty. */
    public val label: String = "",
)

/**
 * ADR-0087 phase B (item 8) — group, channel and publisher links live in the
 * account's synced link register beside contact links, keyed by
 * `groupLinkRecordId(ledgerId)`, so any device of the account lists them and
 * revokes them, and a revoke from any device deletes the ledger row (the
 * record carries the row's revocation seed).
 */
public object GroupLinkRegister {
    private const val TAG = "GroupLinkRegister"

    /** File a freshly uploaded `/g/` link. Best effort: a link the register
     *  did not take still works; this device's own record keeps its seed. */
    public suspend fun record(
        entry: LinkRegisterEntry,
        ledgerId: ByteArray,
        linkSecret: ByteArray,
        revocationSeed: ByteArray,
        capability: ByteArray,
        maxUses: Long,
        /** When the link stops working (unix ms), `0` never. */
        expiryMs: Long = 0L,
    ) {
        val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active ?: return
        try {
            sync.setLinkRecord(
                LinkRecord(
                    hlc = 0uL,
                    linkId = uniffi.lakko_ffi_uniffi.groupLinkRecordId(ledgerId),
                    linkSecret = linkSecret.copyOf(),
                    kind = entry.kind,
                    ledgerId = ledgerId.copyOf(),
                    revocationSeed = revocationSeed.copyOf(),
                    expiryMs = expiryMs.coerceAtLeast(0L).toULong(),
                    maxUses = maxUses.coerceIn(0L, 0xFFFFFFFFL).toUInt(),
                    uses = 0u,
                    revoked = false,
                    mintedAtMs = System.currentTimeMillis().toULong(),
                    label = entry.label,
                    answered = emptyList(),
                    shared = false,
                    editable = false,
                    targetId = entry.targetId.copyOf(),
                    inviteSeed = entry.inviteSeed.copyOf(),
                    admissionVersion = entry.admissionVersion,
                    capability = capability.copyOf(),
                ),
            )
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "a link was not filed in the register: ${e.javaClass.simpleName}")
        }
    }

    /** Whether [record] is a `/g/` link this register lists. */
    public fun isGroupKind(record: LinkRecord): Boolean =
        record.kind == LinkRecordKind.GROUP || record.kind == LinkRecordKind.CHANNEL || record.kind == LinkRecordKind.PUBLISHER

    /**
     * Revoke [record] from this device: the ledger row (signed by the row's
     * revocation key the record carries), a private channel link's invite
     * key at the channels service (on a publisher's device), then the
     * register entry, which every device of the account then reads as
     * revoked. Throws when the row could not be deleted.
     */
    public suspend fun revoke(
        record: LinkRecord,
        channels: ChannelsUiState?,
        channelsClient: ChannelsClient?,
    ) {
        if (record.ledgerId.isNotEmpty() && record.revocationSeed.size == 32) {
            val discovery = me.lakko.app.discovery.DiscoveryClient()
            discovery.setSessionToken(me.lakko.app.auth.SessionTokens.provider?.current())
            discovery.use { it.revokeGroupInvite(record.ledgerId, record.revocationSeed) }
        }
        if (record.kind == LinkRecordKind.CHANNEL && record.inviteSeed.size == 32 && channels != null && channelsClient != null) {
            val channelId = ChannelsComposer.bytesToHex(record.targetId)
            val pub = channels.publisher
            if (pub != null && channels.keysByChannel.value.containsKey(channelId)) {
                try {
                    uniffi.lakko_ffi_uniffi.ChannelInviteSigner.fromInviteSeed(record.inviteSeed).use { signer ->
                        val keyHex = signer.verifyingKey().joinToString("") { "%02x".format(it) }
                        ChannelPublisherSigners.derive(pub, channelId).use { proofs ->
                            channelsClient.deleteInviteKey(channelId, keyHex, proofs)
                        }
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    // The ledger row is gone: nobody can open the link again.
                    Log.w(TAG, "a channel link's invite key was not removed: ${e.javaClass.simpleName}")
                }
            }
        }
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.revokeLinkRecord(record.linkId)
    }
}
