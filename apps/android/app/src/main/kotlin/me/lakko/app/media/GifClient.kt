// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsChannel
import io.ktor.client.statement.bodyAsText
import io.ktor.http.HttpHeaders
import io.ktor.http.encodeURLParameter
import io.ktor.utils.io.readAvailable
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import java.io.ByteArrayOutputStream

/**
 * ADR-0047 §4 — the client half of the GIF proxy (`services/gif-proxy`).
 *
 * Port of `apps/web/src/media/gif-client.ts`, over Ktor like every other
 * Android REST client and routed through [AntiCensorshipEngine] so an armed
 * device reaches the proxy through the obfuscated tunnel too.
 *
 * ## Paths
 *
 * The service's own routes start with `/gifs/`: `/gifs/search?q=&offset=`,
 * `/gifs/trending?offset=` and `/gifs/media/{id}/{preview|full}`. Every request
 * is `baseUrl + <service path>`:
 *
 *   * the default base is `<LAKKO_API_BASE_URL>/gifs` ([ApiEndpoints]); on
 *     the test stack a search is
 *     `https://test.lakko.me/native/gifs/gifs/search?...`;
 *   * local dev ([DEFAULT_BASE_URL], used when the build has no API base URL)
 *     talks to the service directly, so a search is
 *     `http://10.0.2.2:8091/gifs/search?...`;
 *   * production ([PRODUCTION_BASE_URL]) goes through nginx, which strips its
 *     own `/gifs` location prefix, so the same search is
 *     `https://api.lakko.me/gifs/gifs/search?...`.
 *
 * An item's `preview_url` / `full_url` are RELATIVE service paths, joined onto
 * the same base. No URL from a response is used unless it is exactly that
 * relative proxy media shape ([mediaUrl]) — the client never follows a response
 * somewhere else, so recipients' and pickers' requests only ever reach Lakko.
 *
 * ## Failure mapping (UX spec §5 states)
 *
 *   * 503 with body `gif-provider-not-configured` -> [GifResult.NotConfigured]
 *     (the tab hides). A 503 WITHOUT that body is an outage in front of the
 *     service, and hiding the tab for an outage would be wrong.
 *   * 429 + `Retry-After` -> [GifResult.RateLimited]
 *   * 401, or no session token at all -> [GifResult.Unauthorized]
 *   * 502 / anything else / network error -> [GifResult.Unavailable] (Retry)
 *
 * Nothing about a query is logged — the proxy's own rule, kept here.
 */
public class GifClient(
    public val baseUrl: String = ApiEndpoints.baseUrlFor(ApiService.GIFS),
    private val sessionToken: () -> String?,
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) { installTimeouts() }
    } else {
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) { installTimeouts() }
    }

    override fun close() {
        client.close()
    }

    /** The list URL for [query] (trimmed; empty means Trending) at [offset]. */
    public fun listUrl(query: String, offset: Int): String = gifListUrl(baseUrl, query, offset)

    /** [baseUrl] + [relative] when [relative] is exactly a proxy media path. */
    public fun mediaUrl(relative: String?): String? = gifMediaUrl(baseUrl, relative)

    public suspend fun page(query: String, offset: Int): GifResult<GifPage> {
        val token = sessionToken() ?: return GifResult.Unauthorized
        val response = try {
            client.get(listUrl(query, offset)) { bearer(token) }
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            return GifResult.Unavailable
        }
        if (response.status.value !in 200..299) return failureFrom(response)
        val text = try {
            response.bodyAsText()
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            return GifResult.Unavailable
        }
        val page = parseGifPage(text, baseUrl) ?: return GifResult.Unavailable
        return GifResult.Ok(page)
    }

    /**
     * Fetch one rendition through the proxy. Refuses a URL that is not a
     * proxy media URL on [baseUrl], a response whose type is not an allowed
     * media type, and a body over [maxBytes] (read with the cap in force, so an
     * oversize body is never held whole).
     */
    public suspend fun media(url: String, maxBytes: Int): GifResult<GifMedia> {
        if (!url.startsWith(baseUrl.trimEnd('/') + "/gifs/media/")) return GifResult.Unavailable
        val token = sessionToken() ?: return GifResult.Unauthorized
        val response = try {
            client.get(url) { bearer(token) }
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            return GifResult.Unavailable
        }
        if (response.status.value !in 200..299) return failureFrom(response)
        val type = baseMimeOf(response.headers[HttpHeaders.ContentType])
        if (type !in GIF_MEDIA_TYPES) return GifResult.Unavailable
        val declared = response.headers[HttpHeaders.ContentLength]?.toLongOrNull()
        if (declared != null && declared > maxBytes) return GifResult.Unavailable
        val bytes = try {
            readCapped(response, maxBytes)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        } ?: return GifResult.Unavailable
        if (bytes.isEmpty()) return GifResult.Unavailable
        return GifResult.Ok(GifMedia(bytes = bytes, mime = type))
    }

    private suspend fun readCapped(response: HttpResponse, maxBytes: Int): ByteArray? {
        val channel = response.bodyAsChannel()
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(16 * 1024)
        while (true) {
            val read = channel.readAvailable(buffer, 0, buffer.size)
            if (read == -1) break
            if (read == 0) {
                if (channel.isClosedForRead) break
                continue
            }
            if (out.size() + read > maxBytes) return null
            out.write(buffer, 0, read)
        }
        return out.toByteArray()
    }

    private suspend fun failureFrom(response: HttpResponse): GifResult<Nothing> {
        val body = if (response.status.value == 503) {
            try {
                response.bodyAsText()
            } catch (e: CancellationException) {
                throw e
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }
        return failureFromStatus(
            status = response.status.value,
            retryAfter = response.headers[HttpHeaders.RetryAfter],
            body = body,
        )
    }

    private fun io.ktor.client.request.HttpRequestBuilder.bearer(token: String) {
        header(HttpHeaders.Authorization, "Bearer $token")
    }

    private fun io.ktor.client.HttpClientConfig<*>.installTimeouts() {
        install(HttpTimeout) {
            requestTimeoutMillis = REQUEST_TIMEOUT_MS
            connectTimeoutMillis = CONNECT_TIMEOUT_MS
        }
    }

    public companion object {
        /** Android-emulator loopback alias + the service's dev bind — the
         *  fallback when the build carries no API base URL
         *  ([ApiService.GIFS] carries the same value). */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8091"

        /** The production path prefix nginx strips (`/gifs`). */
        public const val PRODUCTION_BASE_URL: String = "https://api.lakko.me/gifs"

        private const val REQUEST_TIMEOUT_MS: Long = 30_000L
        private const val CONNECT_TIMEOUT_MS: Long = 10_000L
    }
}

// =============================================================================
// Pure contract helpers — the rules the tests pin
// =============================================================================

/** UX spec §5 — about 350 ms after typing stops. */
public const val GIF_SEARCH_DEBOUNCE_MS: Long = 350L

/** Mirrors `services/gif-proxy` `MAX_QUERY_CHARS`. */
public const val MAX_GIF_QUERY_CHARS: Int = 100

/** Mirrors the proxy's full-rendition cap. */
public const val MAX_GIF_FULL_BYTES: Int = 8 * 1024 * 1024

/** Mirrors the proxy's preview cap. */
public const val MAX_GIF_PREVIEW_BYTES: Int = 2 * 1024 * 1024

/** ADR-0047 §1 — attachment dimensions are bounded to this. */
public const val MAX_GIF_DIMENSION: Int = 4096

/** Content types the proxy may answer media with (ADR-0047 §4). */
public val GIF_MEDIA_TYPES: List<String> = listOf("image/gif", "image/webp", "video/mp4")

public data class GifItem(
    val id: String,
    /** Absolute URL on the proxy, already joined onto the base. */
    val previewUrl: String,
    val fullUrl: String,
    val width: Int,
    val height: Int,
)

public data class GifPage(val items: List<GifItem>, val nextOffset: Int?)

public data class GifMedia(val bytes: ByteArray, val mime: String) {
    override fun equals(other: Any?): Boolean =
        other is GifMedia && mime == other.mime && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = 31 * bytes.contentHashCode() + mime.hashCode()
}

public sealed interface GifResult<out T> {
    public data class Ok<T>(val value: T) : GifResult<T>

    public data object NotConfigured : GifResult<Nothing>

    public data class RateLimited(val retryAfterSecs: Int) : GifResult<Nothing>

    public data object Unauthorized : GifResult<Nothing>

    public data object Unavailable : GifResult<Nothing>
}

private val MEDIA_PATH = Regex("^/gifs/media/[A-Za-z0-9]{1,64}/(preview|full)$")

private fun trimTrailingSlash(base: String): String = base.trimEnd('/')

public fun gifListUrl(baseUrl: String, query: String, offset: Int): String {
    val base = trimTrailingSlash(baseUrl)
    val trimmed = query.trim()
    val q = if (trimmed.codePointCount(0, trimmed.length) > MAX_GIF_QUERY_CHARS) {
        trimmed.substring(0, trimmed.offsetByCodePoints(0, MAX_GIF_QUERY_CHARS))
    } else {
        trimmed
    }
    val off = if (offset > 0) offset else 0
    if (q.isEmpty()) return "$base/gifs/trending?offset=$off"
    return "$base/gifs/search?q=${q.encodeURLParameter()}&offset=$off"
}

public fun gifMediaUrl(baseUrl: String, relative: String?): String? {
    if (relative == null || !MEDIA_PATH.matches(relative)) return null
    return trimTrailingSlash(baseUrl) + relative
}

private fun positiveDimension(v: JsonPrimitive?): Int? {
    val n = v?.takeIf { !it.isString }?.intOrNull ?: return null
    return n.takeIf { it in 1..MAX_GIF_DIMENSION }
}

private val lenientJson = Json { ignoreUnknownKeys = true }

/**
 * Parse a list response body. Items that do not match the contract are dropped
 * individually; a body that is not a list at all is `null`. The base URL is
 * not known here, so item URLs are joined by [parseGifPage] with a base.
 */
public fun parseGifPage(body: String, baseUrl: String = ""): GifPage? {
    val root = try {
        lenientJson.parseToJsonElement(body) as? JsonObject
    } catch (_: Throwable) {
        null
    } ?: return null
    val rawItems = root["items"] as? JsonArray ?: return null
    val items = rawItems.mapNotNull { element ->
        val obj = element as? JsonObject ?: return@mapNotNull null
        val id = (obj["id"] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
        val preview = gifMediaUrl(baseUrl, (obj["preview_url"] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull)
        val full = gifMediaUrl(baseUrl, (obj["full_url"] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull)
        val width = positiveDimension(obj["width"] as? JsonPrimitive)
        val height = positiveDimension(obj["height"] as? JsonPrimitive)
        if (id == null || preview == null || full == null || width == null || height == null) {
            null
        } else {
            GifItem(id, preview, full, width, height)
        }
    }
    val next = (root["next_offset"] as? JsonPrimitive)
        ?.takeIf { !it.isString }
        ?.longOrNull
        ?.takeIf { it in 1..Int.MAX_VALUE.toLong() }
        ?.toInt()
    return GifPage(items, next)
}

/** Map a non-2xx response to the failure the UI shows. */
public fun failureFromStatus(status: Int, retryAfter: String?, body: String?): GifResult<Nothing> =
    when (status) {
        503 -> if (isNotConfiguredBody(body)) GifResult.NotConfigured else GifResult.Unavailable
        401 -> GifResult.Unauthorized
        429 -> {
            val secs = retryAfter?.trim()?.toDoubleOrNull()
            GifResult.RateLimited(
                if (secs != null && secs.isFinite() && secs > 0) {
                    kotlin.math.ceil(secs).toInt().coerceAtMost(3600)
                } else {
                    1
                },
            )
        }
        else -> GifResult.Unavailable
    }

private fun isNotConfiguredBody(body: String?): Boolean {
    if (body == null) return false
    val obj = try {
        lenientJson.parseToJsonElement(body) as? JsonObject
    } catch (_: Throwable) {
        null
    } ?: return false
    val code = (obj["error"] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
    return code == "gif-provider-not-configured"
}

/** Lowercased type/subtype with parameters removed; empty when absent. */
public fun baseMimeOf(contentType: String?): String {
    if (contentType == null) return ""
    val semi = contentType.indexOf(';')
    return (if (semi == -1) contentType else contentType.substring(0, semi)).trim().lowercase()
}

/**
 * Trailing-edge debounce on a coroutine scope: [schedule] runs the LAST
 * scheduled block once [delayMs] pass with no newer call. [flush] runs a
 * pending block now (the keyboard's search action); [cancel] drops it (the
 * picker closed). Every call is from one thread (the main dispatcher).
 */
public class SearchDebouncer(
    private val scope: CoroutineScope,
    private val delayMs: Long = GIF_SEARCH_DEBOUNCE_MS,
) {
    private var job: Job? = null
    private var pending: (suspend () -> Unit)? = null

    public fun schedule(block: suspend () -> Unit) {
        job?.cancel()
        pending = block
        job = scope.launch {
            delay(delayMs)
            val run = pending
            pending = null
            job = null
            run?.invoke()
        }
    }

    public fun flush() {
        job?.cancel()
        job = null
        val run = pending ?: return
        pending = null
        scope.launch { run() }
    }

    public fun cancel() {
        job?.cancel()
        job = null
        pending = null
    }

    public val hasPending: Boolean get() = pending != null
}

// ----- copy (UX spec §5): catalog keys (ADR-0058) ---------------------------

public const val GIF_PRIVACY_NOTE: String = "chat.gif.privacy_note"
public const val GIF_NO_RESULTS: String = "chat.gif.no_results"
public const val GIF_UNAVAILABLE: String = "chat.gif.unavailable"
public const val GIF_RATE_LIMITED: String = "chat.gif.rate_limited"
public const val GIF_ATTRIBUTION: String = "chat.gif.attribution"
