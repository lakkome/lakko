// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.get
import io.ktor.client.request.parameter
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
import me.lakko.app.auth.base64Decode

/**
 * Ktor-based client for `lakko-svc-ds`.
 *
 * Mirror of `apps/web/src/ds/client.ts`. The Delivery Service is
 * a thin append-only relay: every message goes into either a
 * per-group journal (`/ds/group/{group_hex}`) or a per-mailbox
 * journal (`/ds/mailbox/{device_hex}`). Both paths key the
 * journal by a 32-byte pseudonym encoded as **lowercase hex** in
 * the URL.
 *
 * Two operations per journal kind:
 *
 *   * `POST /ds/{kind}/{pseudonym_hex}/messages` —
 *     body is raw bytes (`application/octet-stream`), server
 *     caps size at its `MAX_MESSAGE_BYTES`. Response is the
 *     assigned monotonic `id` + ISO-8601 `created_at` — the
 *     `id` becomes the `after=` cursor for the next GET.
 *
 *   * `GET /ds/{kind}/{pseudonym_hex}/messages?after=&limit=&wait=`
 *     — pull all messages newer than `after`. `wait` is the
 *     long-poll budget in seconds (0 returns immediately
 *     whatever's there; non-zero holds the connection until at
 *     least one row arrives or the server's `MAX_WAIT_SECS`
 *     clamp kicks in). Response is a JSON envelope carrying the
 *     rows as `payload_b64` strings; we decode to raw bytes
 *     before handing back.
 *
 * ## Auth posture
 *
 * The DS does NOT require PoW or auth tokens in Phase 1 — every
 * write is open to anyone who can reach the URL. Production
 * deployments gate writes on auth-issued tokens (Phase 1.5b+);
 * the client signature stays the same.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8082` — Android-emulator loopback alias, port
 * `8082` matching `lakko-svc-ds`'s dev bind. Override via the
 * constructor for physical devices (LAN IP) and production
 * (Cloudflare tunnel URL).
 */
public class DsClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a
     *  network. */
    engine: HttpClientEngine? = null,
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
        HttpClient(OkHttp) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Mailbox: POST publish
    // -----------------------------------------------------------------

    /**
     * `POST /ds/mailbox/{pseudonym_hex}/messages` — publish a
     * raw payload to a mailbox. Body is sent as
     * `application/octet-stream`; the server caps the size.
     *
     * @param pseudonym 32-byte recipient pseudonym (any of the
     *                  three lifecycle variants from 1.15e2 —
     *                  [me.lakko.app.contacts...] aren't directly
     *                  named here, the bytes are opaque to the DS).
     * @param payload   non-empty bytes to deliver.
     */
    public suspend fun publishMailbox(
        pseudonym: ByteArray,
        payload: ByteArray,
    ): PublishResult {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        return doPublish(
            endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/messages",
            payload = payload,
        )
    }

    /**
     * `POST /ds/group/{group_id_hex}/messages` — publish a raw
     * payload to a group journal.
     *
     * Mirror of [publishMailbox]; the server uses the same FIFO
     * journal shape on either side of the `/group/` vs
     * `/mailbox/` route. Used by the chat surface (1.15f) to
     * ship MLS Application messages.
     *
     * `groupId` is not constrained to 32 bytes here because MLS
     * group ids can be longer (server only requires non-empty);
     * if a caller hands a wrong-shape id the server rejects it
     * with 400.
     */
    public suspend fun publishGroup(
        groupId: ByteArray,
        payload: ByteArray,
    ): PublishResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        return doPublish(
            endpoint = "/ds/group/${pseudonymHex(groupId)}/messages",
            payload = payload,
        )
    }

    /** Shared POST plumbing for the two publish flavours.
     *  Octet-stream body + JSON response. */
    private suspend fun doPublish(endpoint: String, payload: ByteArray): PublishResult {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.post(url) {
                contentType(ContentType.Application.OctetStream)
                setBody(payload)
            }
        } catch (cause: Throwable) {
            throw DsNetworkError(endpoint, cause)
        }
        if (!response.status.isSuccess()) {
            throw DsHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        val parsed: PublishWireResponse = response.body()
        return PublishResult(id = parsed.id, createdAt = parsed.created_at)
    }

    // -----------------------------------------------------------------
    // Mailbox / Group: GET fetch with cursor
    // -----------------------------------------------------------------

    /**
     * `GET /ds/mailbox/{pseudonym_hex}/messages?after=&limit=&wait=`
     * — pull all messages newer than `after` for the given
     * mailbox.
     *
     * `waitSecs` is the long-poll budget. `0` returns immediately
     * (good for catch-up fetches); a non-zero value holds the
     * connection up to that many seconds — typical 30-60 for an
     * active poll loop. The server clamps to its own `MAX_WAIT_SECS`.
     */
    public suspend fun fetchMailbox(
        pseudonym: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
    ): FetchResult {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        return doFetch(
            endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/messages",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
        )
    }

    /**
     * `GET /ds/group/{group_id_hex}/messages?after=&limit=&wait=`
     * — pull all messages newer than `after` for the given group
     * journal. Mirror of [fetchMailbox].
     */
    public suspend fun fetchGroup(
        groupId: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
    ): FetchResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        return doFetch(
            endpoint = "/ds/group/${pseudonymHex(groupId)}/messages",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
        )
    }

    private suspend fun doFetch(
        endpoint: String,
        after: Long?,
        limit: Int?,
        waitSecs: Int?,
    ): FetchResult {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.get(url) {
                configureFetchParams(after = after, limit = limit, waitSecs = waitSecs)
            }
        } catch (cause: Throwable) {
            throw DsNetworkError(endpoint, cause)
        }
        if (!response.status.isSuccess()) {
            throw DsHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        val parsed: FetchWireResponse = response.body()
        return FetchResult(
            messages = parsed.messages.map {
                MailboxMessage(
                    id = it.id,
                    payload = base64Decode(it.payload_b64),
                    createdAt = it.created_at,
                )
            },
            hasMore = parsed.has_more,
        )
    }

    private fun HttpRequestBuilder.configureFetchParams(
        after: Long?,
        limit: Int?,
        waitSecs: Int?,
    ) {
        if (after != null) parameter("after", after.toString())
        if (limit != null) parameter("limit", limit.toString())
        if (waitSecs != null) parameter("wait", waitSecs.toString())
    }

    public companion object {
        /** DS service URL — Android emulator loopback alias, port
         *  `8082` matching the dev `lakko-svc-ds` bind. */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8082"

        /** 32-byte pseudonym (BLAKE3 output length used by every
         *  variant in [me.lakko.app...] — see 1.15e2). */
        private const val PSEUDONYM_LEN: Int = 32

        /**
         * Lowercase-hex encode a byte array (no `0x` prefix).
         * The DS path expects exactly `2 * pseudonym.size`
         * lowercase hex chars. Public because it's a useful
         * helper for tests + for any caller assembling DS URLs
         * directly without going through this client.
         */
        public fun pseudonymHex(bytes: ByteArray): String {
            val sb = StringBuilder(bytes.size * 2)
            for (b in bytes) {
                val v = b.toInt() and 0xFF
                sb.append(HEX[v ushr 4])
                sb.append(HEX[v and 0x0F])
            }
            return sb.toString()
        }

        private const val HEX: String = "0123456789abcdef"
    }
}

// =============================================================================
// Wire shapes (private — public surface uses the cleaner result types below)
// =============================================================================

@Serializable
@Suppress("ConstructorParameterNaming")
private data class PublishWireResponse(
    val id: Long,
    val created_at: String,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class FetchWireResponse(
    val messages: List<FetchWireMessage>,
    val has_more: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class FetchWireMessage(
    val id: Long,
    val payload_b64: String,
    val created_at: String,
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
// Public result types
// =============================================================================

/** Result of [DsClient.publishMailbox] / [DsClient.publishGroup]. */
public data class PublishResult(
    /** Server-assigned monotonic id. Use as the `after` cursor on
     *  subsequent fetches. */
    val id: Long,
    /** ISO-8601 timestamp from the server. */
    val createdAt: String,
)

/** Decoded message from a fetch cursor. */
public data class MailboxMessage(
    val id: Long,
    /** Raw payload bytes (decoded from the server's `payload_b64`). */
    val payload: ByteArray,
    val createdAt: String,
) {
    // Override equals/hashCode for the `payload` ByteArray —
    // same reason as the auth Result types: data-class default
    // is reference equality on arrays, which is rarely what
    // anyone expects.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is MailboxMessage) return false
        return id == other.id &&
            payload.contentEquals(other.payload) &&
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = id.hashCode()
        result = 31 * result + payload.contentHashCode()
        result = 31 * result + createdAt.hashCode()
        return result
    }
}

/** Result of [DsClient.fetchMailbox] / [DsClient.fetchGroup]. */
public data class FetchResult(
    val messages: List<MailboxMessage>,
    /** `true` if there are more rows past the last returned id;
     *  caller should call again with `after = lastMessage.id`. */
    val hasMore: Boolean,
)

// =============================================================================
// Errors
// =============================================================================

/** Programmer error — surfaced before any network call. */
public class DsClientArgError(message: String) : IllegalArgumentException(message)

/** Server returned a non-2xx. */
public class DsHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : RuntimeException("ds $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). */
public class DsNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("ds $endpoint network error: $cause", cause)
