// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import me.lakko.app.chat.ChatRepository
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.composeCallSignaling

/**
 * The process-wide 1x1 call host.
 *
 * A process object rather than anything owned by a screen or a view model: a
 * call must outlive leaving the chat it started in, keep ringing on the chat
 * list, home or contacts, and survive an activity recreate. `MainActivity`
 * attaches the current [ChatRepository] through [ChatRepositoryCallPort] and
 * points the repository's one-to-one inbound hook at [OneToOneCallHost.deliver].
 *
 * Runs on the main dispatcher: [CallSession] mutates its state flows there and
 * its entry points were always called from the main thread.
 */
public object AppCallHost {
    public val instance: OneToOneCallHost<CallSession> by lazy {
        OneToOneCallHost(scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate))
    }
}

/** The production [OneToOneCallPort]: real sessions, the MLS exporter, the DS journal. */
public class ChatRepositoryCallPort(
    private val applicationContext: Context,
    private val repository: ChatRepository,
) : OneToOneCallPort<CallSession> {

    override fun createSession(
        groupIdHex: String,
        callId: ByteArray,
        outbound: (ByteArray) -> Unit,
    ): CallSession = CallSession(
        applicationContext = applicationContext,
        callId = callId,
        onOutboundSignaling = outbound,
    )

    override suspend fun deriveCallKey(groupIdHex: String, callId: ByteArray): OneToOneCallKey? =
        repository.deriveCallKey(groupIdHex = groupIdHex, callId = callId)
            ?.let { OneToOneCallKey(seed = it.seed, localLeaf = it.localLeaf) }

    override suspend fun publish(groupIdHex: String, bytes: ByteArray) {
        repository.publishApplicationMessage(groupIdHex = groupIdHex, plaintextBytes = bytes)
    }

    override fun composeEnd(callId: ByteArray, reason: FfiCallEndReason): ByteArray? =
        try {
            composeCallSignaling(FfiCallSignalingBody.End(callId = callId, reason = reason))
        } catch (error: Throwable) {
            android.util.Log.w("ChatRepositoryCallPort", "compose End failed", error)
            null
        }
}
