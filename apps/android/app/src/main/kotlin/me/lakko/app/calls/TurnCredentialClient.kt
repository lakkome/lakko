// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.util.Log
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.webrtc.PeerConnection

/**
 * The TURN credential a call's ICE configuration adds (G3-{1,2}-turn), from
 * the anonymous call-admission exchange (ADR-0078 section 2). Mirror of the
 * iOS `TurnCredentialClient.swift` and the web `turnProviderForCall`.
 *
 * There is no bearer route for TURN any more: a credential is minted beside
 * an SFU call token by `POST /call/jwt`, which spends one anonymous
 * `call-join` token, and its username tag is random per credential, so the
 * relay (and its logs) never see an account. An SFU call already holds one
 * ([preMinted]); a one-to-one call, which needs no SFU token, mints one for
 * its call id the same way ([forCall]).
 *
 * **Fail-soft by contract.** No credential (a deployment without a relay, a
 * network failure, no token to be had) resolves to `null` and the call
 * proceeds on STUN alone: an availability choice about ICE candidates, never
 * an E2EE fallback (invariant #4 is untouched). A cancelled caller is not
 * answered: the cancellation is passed on.
 */
public class TurnCredentialClient private constructor(
    private val mint: suspend () -> TurnCredential?,
) {
    private val lock = Mutex()
    private var minted: CompletableDeferred<TurnCredential?>? = null

    /** The ICE server to add, or `null` for STUN-only. Minted at most once. */
    public suspend fun fetchOrNull(): PeerConnection.IceServer? {
        val credential = try {
            credential()
        } catch (cause: Throwable) {
            // A call that ended while the credential was being fetched has
            // cancelled its start. That is not "no TURN": answering `null`
            // here let the cancelled start run on and open a peer connection
            // and the camera for a call that was already over.
            me.lakko.app.rethrowIfCancelled(cause)
            Log.w(TAG, "mint failed, proceeding STUN-only")
            null
        } ?: return null
        return PeerConnection.IceServer.builder(credential.urls)
            .setUsername(credential.username)
            .setPassword(credential.password)
            .createIceServer()
    }

    private suspend fun credential(): TurnCredential? {
        // A cancelled start spends no token.
        currentCoroutineContext().ensureActive()
        val existing = lock.withLock { minted }
        if (existing != null) return existing.await()
        val value = mint()
        lock.withLock {
            val again = minted
            if (again != null) return again.await()
            minted = CompletableDeferred(value)
        }
        return value
    }

    public companion object {
        private const val TAG = "lakko.turn"

        /** A credential known up front (minted beside an SFU call token). */
        public fun preMinted(credential: TurnCredential?): TurnCredentialClient =
            TurnCredentialClient { credential }

        /** A credential minted once, on first use, for the 16-byte [callId]
         *  through the anonymous call-join exchange. */
        public fun forCall(
            callId: ByteArray,
            jwtClient: () -> CallJwtClient = { CallJwtClient() },
        ): TurnCredentialClient = TurnCredentialClient {
            jwtClient().use { it.mint(callId).turn }
        }
    }
}
