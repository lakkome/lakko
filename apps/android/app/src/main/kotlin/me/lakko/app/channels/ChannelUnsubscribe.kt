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
     */
    public fun leave(state: ChannelsUiState, channelId: String, baseUrl: String) {
        val keys = state.subscriberKeysByChannel.value[channelId]?.let {
            ChannelSubscriberKeys(secret = it.secret.copyOf(), publicKey = it.publicKey.copyOf())
        }
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        // ADR-0060 §3 — the server drops the member card with the
        // subscription, so a later subscription uploads afresh.
        ChannelMemberCards.forgetUploads(channelId)
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
