// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.sendWithToken
import kotlin.coroutines.cancellation.CancellationException

/**
 * Ktor-based client for `lakko-svc-push-gw`.
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/client.ts`
 *   * `apps/ios/Lakko/Push/PushClient.swift`
 *
 * Since ADR-0078 section 3 push-gw holds no device pseudonym and no token in
 * the clear. Endpoints used from the device:
 *
 *   * `POST /push/subscribe` — store (or replace) one sealed subscription
 *     `{sub_id, push_gid, wake_pk, sealed}`: `sealed` is this device's tokens
 *     sealed under the group's unlock key (`sealPushSubscription`), `sub_id`
 *     16 random bytes this device keeps per subscription ([PushRouteStore]).
 *     Re-sending an id replaces its row, which is how a new token reaches
 *     push-gw.
 *   * `POST /push/unsubscribe` — drop one row by its `sub_id`.
 *   * `GET /push/wake-key` — the X25519 key every wake is sealed to.
 *
 * The two writes spend one `push-register` anonymous token each (ADR-0054
 * S3) and go out on the anonymous client: no session, no cookie, its own
 * connection pool (invariant #5).
 *
 * It never WAKES anyone: a group wake is the `x-lakko-wake` header on the
 * signed DS publish (see `me.lakko.app.ds.DsWake`).
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8085` — Android-emulator loopback alias, port `8085`
 * matching `lakko-svc-push-gw`'s dev bind. Override via the constructor.
 */
public class PushClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: [AnonymousHttp.engine] (its own pool). Tests:
     *  `MockEngine` for deterministic responses without a network. */
    engine: HttpClientEngine? = null,
    /** Where the two writes draw their `push-register` token. */
    private val anonTokens: AnonTokenSource = AnonTokens.shared,
) : AutoCloseable {

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) {
            install(ContentNegotiation) { json(json) }
        }
    } else {
        HttpClient(AnonymousHttp.engine) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    /** Store (or replace) one sealed subscription (ADR-0078). */
    public suspend fun subscribe(
        subId: ByteArray,
        pushGid: ByteArray,
        wakePk: ByteArray,
        sealed: ByteArray,
    ) {
        require(subId.size == SUBSCRIPTION_ID_LEN) { "subId must be $SUBSCRIPTION_ID_LEN bytes" }
        require(pushGid.size == KEY_LEN && wakePk.size == KEY_LEN) {
            "pushGid and wakePk must be $KEY_LEN bytes"
        }
        require(sealed.isNotEmpty()) { "sealed must be non-empty" }
        val req = SubscribeRequest(
            sub_id = b64url(subId),
            push_gid = b64url(pushGid),
            wake_pk = b64url(wakePk),
            sealed = b64url(sealed),
        )
        val response: HttpResponse = tokenPost("/push/subscribe") { setBody(req) }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/subscribe",
                body = response.safeReadBody(),
            )
        }
        if (!response.body<OkResponse>().ok) {
            throw PushHttpError(status = 200, endpoint = "/push/subscribe", body = "server reported ok=false")
        }
    }

    /** Drop one subscription by its id. Idempotent: `false` when it was
     *  already gone. */
    public suspend fun unsubscribe(subId: ByteArray): Boolean {
        require(subId.size == SUBSCRIPTION_ID_LEN) { "subId must be $SUBSCRIPTION_ID_LEN bytes" }
        val response: HttpResponse = tokenPost("/push/unsubscribe") {
            setBody(UnsubscribeRequest(sub_id = b64url(subId)))
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/unsubscribe",
                body = response.safeReadBody(),
            )
        }
        return response.body<UnsubscribeResponse>().removed
    }

    /** push-gw's X25519 wake key. No token: it is public. */
    public suspend fun wakeKey(): ByteArray {
        val response: HttpResponse = try {
            client.get("$baseUrl/push/wake-key")
        } catch (cause: CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/wake-key", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(response.status.value, "/push/wake-key", response.safeReadBody())
        }
        val key = runCatching {
            java.util.Base64.getUrlDecoder().decode(response.body<WakeKeyResponse>().key)
        }.getOrNull()
        if (key == null || key.size != KEY_LEN) {
            throw PushHttpError(response.status.value, "/push/wake-key", "malformed key")
        }
        return key
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    /** One POST with a `push-register` token, one retry on a token refusal,
     *  never a session. */
    private suspend fun tokenPost(
        endpoint: String,
        body: io.ktor.client.request.HttpRequestBuilder.() -> Unit,
    ): HttpResponse =
        anonTokens.sendWithToken(AnonTokenContext.PUSH_REGISTER) { name, value ->
            try {
                client.post("$baseUrl$endpoint") {
                    header(name, value)
                    contentType(ContentType.Application.Json)
                    body()
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                throw PushNetworkError(endpoint, cause)
            }
        }

    public companion object {
        /** Default URL — `<LAKKO_API_BASE_URL>/push-gw` ([ApiEndpoints]),
         *  or the Android emulator loopback alias + the dev
         *  `lakko-svc-push-gw` bind (`http://10.0.2.2:8085`) without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.PUSH_GW)

        /** Length of a subscription id (mirror of `PUSH_SUBSCRIPTION_ID_LEN`). */
        public const val SUBSCRIPTION_ID_LEN: Int = 16

        /** Length of a `push_gid`, a wake public key and push-gw's wake key. */
        public const val KEY_LEN: Int = 32

        /** Lowercase-hex encode bytes (route-store keys, subscription ids). */
        public fun pseudonymHex(bytes: ByteArray): String {
            val sb = StringBuilder(bytes.size * 2)
            for (b in bytes) {
                val v = b.toInt() and 0xFF
                sb.append(HEX[v ushr 4])
                sb.append(HEX[v and 0x0F])
            }
            return sb.toString()
        }

        private fun b64url(bytes: ByteArray): String =
            java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

        private const val HEX: String = "0123456789abcdef"
    }
}

// =============================================================================
// Wire shapes
// =============================================================================

@Serializable
@Suppress("ConstructorParameterNaming")
private data class SubscribeRequest(
    val sub_id: String,
    val push_gid: String,
    val wake_pk: String,
    val sealed: String,
)

@Serializable
private data class OkResponse(
    val ok: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class UnsubscribeRequest(
    val sub_id: String,
)

@Serializable
private data class UnsubscribeResponse(
    val ok: Boolean,
    val removed: Boolean,
)

@Serializable
private data class WakeKeyResponse(
    val key: String,
)

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}

// =============================================================================
// Errors
// =============================================================================

/** Server returned a non-2xx. */
public class PushHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : RuntimeException("push $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). */
public class PushNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("push $endpoint network error: $cause", cause)
