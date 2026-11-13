// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Log
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import uniffi.lakko_ffi_uniffi.ChannelSubscriberKeypair

/**
 * ADR-0060 §3 — a subscriber leaves a channel: the server is told with a
 * `DELETE /v1/channels/{id}/subscribers/{pk}` signed by the subscriber key,
 * and this device forgets the channel whatever the server answered.
 */
public object ChannelUnsubscribe {
    /** Outlives the screen that asked: leaving navigates away at once, and
     *  the request must not be cancelled with it. */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * Tell the server: sign with [keys] (the subscriber key registered for
     * [channelIdHex]) and send the DELETE through [client]. Throws on a
     * network or server failure; a `404` is done.
     *
     * @param sign produces the header value; the default is the core's
     *   `ChannelSubscriberKeypair.unsubscribeRequestHeader` (a host test
     *   passes its own).
     * @param headerName the header the value rides in; the default is the
     *   core's `unsubscribeSigHeaderName()`.
     */
    public suspend fun tellServer(
        client: ChannelsClient,
        channelIdHex: String,
        keys: ChannelSubscriberKeys,
        nowMs: Long = System.currentTimeMillis(),
        headerName: String = uniffi.lakko_ffi_uniffi.unsubscribeSigHeaderName(),
        sign: (secret: ByteArray, channelId: ByteArray, issuedAtMs: ULong) -> String = ::coreSignature,
    ) {
        val header = sign(
            keys.secret,
            ChannelsComposer.channelIdFromHex(channelIdHex),
            nowMs.coerceAtLeast(0L).toULong(),
        )
        client.unsubscribe(
            channelIdHex = channelIdHex,
            subscriberPubkeyHex = ChannelsComposer.bytesToHex(keys.publicKey),
            signatureHeaderName = headerName,
            signatureHeaderValue = header,
        )
    }

    /**
     * Leave [channelId]: drop this device's local state for it at once, then
     * tell the server in the background with a client on [baseUrl]. A failure
     * to reach the server is logged; the local state is gone either way (the
     * server's 90-day prune retires a row nobody reads).
     *
     * ADR-0087 section 9.5: the row and its posts go from every device of the
     * person. [tellDevices] says so over the DeviceGroup (the conversation
     * end and the channel subscription register's leave, written BEFORE the
     * channel goes so no reconcile adds it back); a device acting on either
     * register passes `false`. [tellServer] `false`: the device that left
     * already told the service (docs/ux/second-device.md section 4a).
     */
    public fun leave(
        state: ChannelsUiState,
        channelId: String,
        baseUrl: String,
        tellDevices: Boolean = true,
        tellServer: Boolean = true,
    ) {
        if (tellDevices) ChannelSubscriptionSync.recordLeave(channelId)
        val keys = if (!tellServer) {
            null
        } else {
            state.subscriberKeysByChannel.value[channelId]?.let {
                ChannelSubscriberKeys(secret = it.secret.copyOf(), publicKey = it.publicKey.copyOf())
            }
        }
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        // ADR-0060 §3 — the server drops the member card with the
        // subscription, so a later subscription uploads afresh.
        ChannelMemberCards.forgetUploads(channelId)
        // The post log keeps only the channels held, at its next sync.
        me.lakko.app.feed.requestChannelPostSync()
        channelRef(channelId)?.let { ref ->
            scope.launch {
                try {
                    val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
                    if (tellDevices) sync?.publishConversationEnded(ref, 0u)
                    sync?.clearConversationRegisters(ref)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "unsubscribe not told to the other devices: ${cause.javaClass.simpleName}")
                }
            }
        }
        if (keys == null) return
        scope.launch {
            try {
                ChannelsClient(baseUrl).use { client -> tellServer(client, channelId, keys) }
                Log.i(LOG_TAG, "unsubscribed on the server")
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "unsubscribe not sent to the server: ${cause.javaClass.simpleName}")
            } finally {
                keys.secret.fill(0)
            }
        }
    }

    /**
     * This device follows [channelId] again: tell the account's other devices,
     * or the register keeps saying it left. A first subscription finds no
     * register and writes none.
     */
    public fun noteFollowing(channelId: String) {
        val ref = channelRef(channelId) ?: return
        scope.launch {
            try {
                me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.publishConversationJoined(ref, 0u)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "following not told to the other devices: ${cause.javaClass.simpleName}")
            }
        }
    }

    /**
     * ADR-0087 section 9.5 - what a register that says this account left a
     * channel changes on this device: the channels to drop, given what it
     * follows. A channel this device publishes is not a subscription.
     */
    internal fun channelsToLeave(
        ended: List<me.lakko.app.chat.HeardConversationEnd>,
        followed: Set<String>,
        published: Set<String>,
    ): List<String> = ended.filter { it.channel }.map { it.idHex }
        .filter { id -> followed.any { it.equals(id, ignoreCase = true) } && published.none { it.equals(id, ignoreCase = true) } }

    /** The reference a register names [channelId] with. */
    private fun channelRef(channelId: String): uniffi.lakko_ffi_uniffi.FolderConversationRef? =
        me.lakko.app.chat.bytesOfHex(channelId)?.let {
            uniffi.lakko_ffi_uniffi.FolderConversationRef(
                kind = uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL,
                id = it,
            )
        }

    private fun coreSignature(secret: ByteArray, channelId: ByteArray, issuedAtMs: ULong): String {
        val keypair = ChannelSubscriberKeypair.fromSecret(secret)
        try {
            return keypair.unsubscribeRequestHeader(channelId, issuedAtMs)
        } finally {
            keypair.close()
        }
    }

    private const val LOG_TAG = "ChannelUnsubscribe"
}
