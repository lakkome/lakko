// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.client.statement.readRawBytes
import io.ktor.http.ContentType
import io.ktor.http.contentType
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.lakkoHttpEngine

/**
 * ADR-0099 section 2 — the moderation service, sessionless.
 *
 * Every request rides the shared Lakko HTTP engine, so an armed device sends
 * it through the anti-censorship tunnel like every other service. A report
 * carries one anonymous `report` token and nothing that names the account.
 * Nothing about a report is logged.
 */
public class ModerationClient(
    public val baseUrl: String = ApiEndpoints.baseUrlFor(ApiService.MODERATION),
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val client: HttpClient = HttpClient(engine ?: lakkoHttpEngine()) {
        install(HttpTimeout) {
            requestTimeoutMillis = REQUEST_TIMEOUT_MS
            connectTimeoutMillis = CONNECT_TIMEOUT_MS
        }
        expectSuccess = false
    }

    override fun close() {
        client.close()
    }

    /** `GET /moderation/moderators`: the signed list, or why there is none. */
    public suspend fun moderatorList(): ModeratorListFetch {
        val response = request { client.get("$baseUrl/moderation/moderators") } ?: return ModeratorListFetch.Network
        return when (response.status.value) {
            in 200..299 -> {
                val bytes = readOrNull { response.readRawBytes() } ?: return ModeratorListFetch.Network
                if (bytes.isEmpty()) ModeratorListFetch.Unavailable else ModeratorListFetch.Ok(bytes)
            }
            404, 503 -> ModeratorListFetch.Unavailable
            else -> ModeratorListFetch.Network
        }
    }

    /** `POST /moderation/report` with one `report` token. */
    public suspend fun postReport(report: ByteArray, tokenHeaderName: String, tokenHeaderValue: String): ReportPost {
        val response = request {
            client.post("$baseUrl/moderation/report") {
                header(tokenHeaderName, tokenHeaderValue)
                contentType(ContentType.Application.OctetStream)
                setBody(report)
            }
        } ?: return ReportPost.Network
        val text = readOrNull { response.bodyAsText() }
        val status = response.status.value
        if (status in 200..299) {
            val receipt = jsonField(text, "receipt_id") ?: return ReportPost.Refused(status, null)
            return ReportPost.Created(receipt)
        }
        return ReportPost.Refused(status, jsonField(text, "error"))
    }

    /** `GET /moderation/report/{id}`: the report's status, or `null` when unknown or unreachable. */
    public suspend fun reportStatus(receiptId: String): ReportStatus? {
        if (!RECEIPT_ID.matches(receiptId)) return null
        val response = request { client.get("$baseUrl/moderation/report/$receiptId") } ?: return null
        if (response.status.value !in 200..299) return null
        return ReportStatus.fromWire(jsonField(readOrNull { response.bodyAsText() }, "status"))
    }

    /**
     * ADR-0099 step 4 — `GET /moderation/snapshot/latest/bucket/{prefix}`, no
     * session: the latest suspension snapshot's bucket for one four-hex
     * prefix, as the JSON body the core verifies; `null` on any failure.
     */
    public suspend fun snapshotBucket(prefix: String): String? {
        if (!BUCKET_PREFIX.matches(prefix)) return null
        val response = request { client.get("$baseUrl/moderation/snapshot/latest/bucket/$prefix") } ?: return null
        if (response.status.value !in 200..299) return null
        return readOrNull { response.bodyAsText() }?.takeIf { it.isNotBlank() && it.length <= MAX_SNAPSHOT_BODY_CHARS }
    }

    /**
     * ADR-0099 F2 — `GET /moderation/log/entry/{index}?size={size}`, no
     * session: one moderation log leaf with its inclusion path in the tree of
     * [size] leaves (the bucket answer's own log size), as the JSON body the
     * core proves a standing with; `null` on any failure.
     */
    public suspend fun logEntry(index: ULong, size: ULong): String? {
        val response = request { client.get("$baseUrl/moderation/log/entry/$index?size=$size") } ?: return null
        if (response.status.value !in 200..299) return null
        return readOrNull { response.bodyAsText() }?.takeIf { it.isNotBlank() && it.length <= MAX_SNAPSHOT_BODY_CHARS }
    }

    private suspend fun request(send: suspend () -> HttpResponse): HttpResponse? = try {
        send()
    } catch (cause: CancellationException) {
        throw cause
    } catch (_: Throwable) {
        null
    }

    private suspend fun <T> readOrNull(read: suspend () -> T): T? = try {
        read()
    } catch (cause: CancellationException) {
        throw cause
    } catch (_: Throwable) {
        null
    }

    public companion object {
        private const val REQUEST_TIMEOUT_MS: Long = 30_000L
        private const val CONNECT_TIMEOUT_MS: Long = 10_000L

        /** The core refuses a body over 2 MiB; nothing larger is handed to it. */
        private const val MAX_SNAPSHOT_BODY_CHARS: Int = 2 * 1024 * 1024

        /** A receipt id: 16 bytes in unpadded base64url, 22 characters. */
        internal val RECEIPT_ID: Regex = Regex("^[A-Za-z0-9_-]{22}$")
    }
}

/** A report's status as the service answers it (UX section 2.3, Your reports). */
public enum class ReportStatus(public val wire: String, public val key: String) {
    RECEIVED("received", "report.status.received"),
    ACTION("action", "report.status.action"),
    NONE("none", "report.status.none"),
    ;

    public companion object {
        public fun fromWire(value: String?): ReportStatus? = entries.firstOrNull { it.wire == value }
    }
}

/** One string field of a JSON object body, or `null`. */
internal fun jsonField(body: String?, name: String): String? {
    if (body.isNullOrBlank()) return null
    return try {
        ((Json.parseToJsonElement(body) as? JsonObject)?.get(name) as? JsonPrimitive)?.contentOrNull
    } catch (_: Throwable) {
        null
    }
}
