// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import org.webrtc.PeerConnection
import java.net.HttpURLConnection
import java.net.URL

/**
 * Client for `POST /call/turn-credential` (G3-{1,2}-turn step 2).
 *
 * Mirror of the iOS `TurnCredentialClient.swift`. Fetches a
 * short-lived TURN credential at call-start and surfaces it
 * as an [`PeerConnection.IceServer`] row ready to splice into
 * the [`PeerConnection.RTCConfiguration.iceServers`] list.
 *
 * ## Fail-soft posture
 *
 * A 503 response (`turn_not_configured` body) is the
 * **expected** state for STUN-only deployments — the caller
 * proceeds with the STUN-only ICE list returned by
 * [`WebRtcPeerConnection.Companion.defaultIceServers`]. A
 * genuine transport failure (network down, 5xx other than
 * 503) is also collapsed to "no TURN this call" because
 * the alternative — refusing to dial — is much worse UX than
 * a degraded-NAT-traversal call. The single situation we
 * **don't** fail-soft on is a 200 with malformed body —
 * that indicates a deployment mismatch the caller should
 * know about.
 */
public class TurnCredentialClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
) {
    public companion object {
        /** The auth service base: `<LAKKO_API_BASE_URL>/auth`
         *  ([ApiEndpoints]), or `http://10.0.2.2:8081` without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.AUTH)
        private const val TAG = "lakko.turn"

        private val json: Json = Json {
            ignoreUnknownKeys = true
        }
    }

    @Volatile
    private var sessionToken: String? = null

    /** Rebind the bearer token. `null` clears. */
    public fun setSessionToken(token: String?) {
        sessionToken = token
    }

    /**
     * Fail-soft fetch. Returns `null` on any of:
     *   * 503 ("TURN not configured on this deployment")
     *   * transport / HTTP error
     *   * missing session token
     *
     * A cancelled caller is not answered: the cancellation is passed on.
     *
     * Logs the underlying cause at WARN with the `lakko.turn`
     * tag so the audit pack can grep for it.
     */
    public suspend fun fetchOrNull(): PeerConnection.IceServer? {
        return try {
            fetch()
        } catch (cause: Throwable) {
            // A call that ended while the credential was being fetched has
            // cancelled its start. That is not "no TURN": answering `null`
            // here let the cancelled start run on and open a peer connection
            // and the camera for a call that was already over.
            me.lakko.app.rethrowIfCancelled(cause)
            Log.w(TAG, "fetch failed, proceeding STUN-only", cause)
            null
        }
    }

    /**
     * Stricter variant — surfaces every error. Use in tests;
     * production code wants [`fetchOrNull`] for the
     * fail-soft posture.
     */
    public suspend fun fetch(): PeerConnection.IceServer? = withContext(Dispatchers.IO) {
        val token = sessionToken
            ?: throw TurnCredentialClientException.Argument(
                "session token is not bound — call setSessionToken() before fetch()"
            )

        val url = URL("$baseUrl/call/turn-credential")
        val conn = (url.openConnection() as HttpURLConnection).apply {
            requestMethod = "POST"
            setRequestProperty("Authorization", "Bearer $token")
            setRequestProperty("Accept", "application/json")
            // No body — the endpoint binds the credential to
            // the bearer-derived account_id, not to anything
            // we send.
            connectTimeout = 10_000
            readTimeout = 10_000
        }
        try {
            val status = conn.responseCode
            if (status == HttpURLConnection.HTTP_UNAVAILABLE) {
                // 503 = TURN not configured on this service.
                // Body is `{"error":"turn_not_configured",...}`;
                // we don't introspect it.
                return@withContext null
            }
            if (status !in 200..299) {
                throw TurnCredentialClientException.Network(
                    endpoint = "/call/turn-credential",
                    status = status,
                )
            }
            val body = conn.inputStream.use { it.readBytes() }.decodeToString()
            val parsed = try {
                json.decodeFromString<TurnCredentialResponse>(body)
            } catch (cause: Throwable) {
                throw TurnCredentialClientException.Decode(cause.message ?: "decode failed")
            }
            PeerConnection.IceServer.builder(parsed.urls)
                .setUsername(parsed.username)
                .setPassword(parsed.password)
                .createIceServer()
        } finally {
            conn.disconnect()
        }
    }
}

/** JSON wire-shape mirroring the Rust `TurnCredentialResponse`. */
@Serializable
internal data class TurnCredentialResponse(
    val urls: List<String>,
    val username: String,
    val password: String,
    @SerialName("ttl_secs")
    val ttlSecs: Long,
    @SerialName("expiry_unix_secs")
    val expiryUnixSecs: Long,
)

public sealed class TurnCredentialClientException(message: String) : Exception(message) {
    public class Argument(message: String) : TurnCredentialClientException(message)
    public class Network(endpoint: String, status: Int) :
        TurnCredentialClientException("HTTP $status from $endpoint")
    public class Decode(reason: String) :
        TurnCredentialClientException("malformed body: $reason")
}
