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

    /** How the account's other devices learn of a leave ([tellDevices]
     *  `false`: only this device's registers are cleared). Tests replace it. */
    @Volatile
    internal var tellOtherDevices: suspend (uniffi.lakko_ffi_uniffi.FolderConversationRef, Boolean) -> Unit =
        { ref, tellDevices ->
            val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
            if (tellDevices) sync?.publishConversationEnded(ref, 0u)
            sync?.clearConversationRegisters(ref)
        }

    /** The client the background unsubscribe goes through (tests replace it). */
    @Volatile
    internal var serverClient: (String) -> ChannelsClient = { ChannelsClient(it) }

    /** The unsubscribe signature (tests replace the core's). */
    @Volatile
    internal var serverSign: (ByteArray, ByteArray, ULong) -> String = ::coreSignature

    /** The unsubscribe signature header's name (tests replace the core's). */
    @Volatile
    internal var serverHeaderName: () -> String = { uniffi.lakko_ffi_uniffi.unsubscribeSigHeaderName() }

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
     * ADR-0089 step 5 — the owner or a publisher takes a channel moderators
     * removed out of the list: it ends here and the account's conversation
     * end is written, so the account's other devices drop it too, as a
     * subscriber's Unsubscribe does. Only the channels-service call is
     * skipped (every route answers `410 channel-removed`).
     */
    public fun leaveRemoved(state: ChannelsUiState, channelId: String) {
        leave(state, channelId, baseUrl = "", tellDevices = true, callService = false)
    }

    /**
     * Leave [channelId]: drop this device's local state for it at once, then
     * tell the server in the background with a client on [baseUrl]. A failure
     * to reach the server is logged; the local state is gone either way (the
     * server's 90-day prune retires a row nobody reads).
     *
     * ADR-0087 section 9.5: the row and its posts go from every device of the
     * person. [tellDevices] says so over the DeviceGroup; a device acting on
     * that register passes `false`.
     */
    public fun leave(
        state: ChannelsUiState,
        channelId: String,
        baseUrl: String,
        tellDevices: Boolean = true,
        /** `false`: no channels-service call ([leaveRemoved]). */
        callService: Boolean = true,
    ) {
        val keys = state.subscriberKeysByChannel.value[channelId]?.let {
            ChannelSubscriberKeys(secret = it.secret.copyOf(), publicKey = it.publicKey.copyOf())
        }
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        // ADR-0089 step 5 — a removed channel's mark goes with it.
        RemovedChannels.noteAnswered(channelId)
        // ADR-0060 §3 — the server drops the member card with the
        // subscription, so a later subscription uploads afresh.
        ChannelMemberCards.forgetUploads(channelId)
        // The post log keeps only the channels held, at its next sync.
        me.lakko.app.feed.requestChannelPostSync()
        channelRef(channelId)?.let { ref ->
            scope.launch {
                try {
                    tellOtherDevices(ref, tellDevices)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    runCatching { Log.w(LOG_TAG, "unsubscribe not told to the other devices: ${cause.javaClass.simpleName}") }
                }
            }
        }
        if (keys == null) return
        if (!callService) {
            keys.secret.fill(0)
            return
        }
        scope.launch {
            try {
                serverClient(baseUrl).use { client ->
                    tellServer(client, channelId, keys, headerName = serverHeaderName(), sign = serverSign)
                }
                runCatching { Log.i(LOG_TAG, "unsubscribed on the server") }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                runCatching { Log.w(LOG_TAG, "unsubscribe not sent to the server: ${cause.javaClass.simpleName}") }
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
