// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.add
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService

/**
 * ADR-0090 section 4 — one vote per account in a channel poll, through an
 * anonymous credential (`lakko-anon-cred`) issued by auth once per account and
 * period, presented to the channels service with a per-poll tag.
 */

/** What the channels service answered a vote write. */
public sealed interface ChannelVoteAnswer {
    public data object Stored : ChannelVoteAnswer
    /** 409 `stale-vote`: a later vote by this account is stored. */
    public data class Stale(val storedAtMs: Long) : ChannelVoteAnswer
    /** 400 `vote-time-out-of-window`: `at_ms` too far from the server clock. */
    public data class OutOfWindow(val serverTimeMs: Long) : ChannelVoteAnswer
    public data class Refused(val status: Int, val code: String?) : ChannelVoteAnswer
}

/** The server's tolerance on `at_ms` (ADR-0090 section 4.3). */
public const val VOTE_WINDOW_MS: Long = 120_000L

/** How many writes one vote may take (first try + stale / clock retries). Chosen. */
public const val VOTE_MAX_ATTEMPTS: Int = 3

/**
 * The vote loop: write at [nowMs]; on `stale-vote` retry at `stored_at_ms + 1`
 * while that stays inside the window around now; on a clock refusal retry at
 * the server's time. [write] signs and sends one attempt at the given `at_ms`.
 * Returns whether the vote was stored. Pure apart from [write] (host-tested).
 */
public suspend fun castChannelVote(
    nowMs: () -> Long,
    write: suspend (atMs: Long) -> ChannelVoteAnswer,
): Boolean {
    var at = nowMs()
    repeat(VOTE_MAX_ATTEMPTS) {
        when (val answer = write(at)) {
            ChannelVoteAnswer.Stored -> return true
            is ChannelVoteAnswer.Stale -> {
                val next = answer.storedAtMs + 1
                if (kotlin.math.abs(next - nowMs()) > VOTE_WINDOW_MS) return false
                at = next
            }
            is ChannelVoteAnswer.OutOfWindow -> at = answer.serverTimeMs
            is ChannelVoteAnswer.Refused -> return false
        }
    }
    return false
}

/** The service's error body `{"code","detail",...}` and one numeric field. */
internal fun voteAnswerOf(status: Int, body: String?): ChannelVoteAnswer {
    if (status in 200..299) return ChannelVoteAnswer.Stored
    val obj = runCatching { Json.parseToJsonElement(body ?: "") as? JsonObject }.getOrNull()
    val code = obj?.get("code")?.jsonPrimitive?.content
    fun long(name: String): Long? = obj?.get(name)?.jsonPrimitive?.longOrNull
    return when {
        status == 409 && code == "stale-vote" -> long("stored_at_ms")?.let { ChannelVoteAnswer.Stale(it) }
            ?: ChannelVoteAnswer.Refused(status, code)
        status == 400 && code == "vote-time-out-of-window" -> long("server_time_ms")?.let { ChannelVoteAnswer.OutOfWindow(it) }
            ?: ChannelVoteAnswer.Refused(status, code)
        else -> ChannelVoteAnswer.Refused(status, code)
    }
}

/**
 * The credential of the current period, kept on this device per account and
 * period (a credential is bound to the account's lineage seed and good for
 * one period); asked of auth the first time a person votes in a period.
 */
public class ChannelPollCredentials(
    private val context: Context,
    private val accountIdB64: () -> String?,
    private val bearer: () -> String?,
    private val lineageSeed: () -> ByteArray?,
    private val baseUrl: String = ApiEndpoints.baseUrlFor(ApiService.AUTH),
) {
    private val client = HttpClient(me.lakko.app.transport.lakkoHttpEngine())
    private val lock = Mutex()
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** The credential for [period] and the issuer key it verifies under, or `null`. */
    public suspend fun credentialFor(period: ULong): Pair<ByteArray, uniffi.lakko_ffi_uniffi.AnonIssuerPublicKey>? = lock.withLock {
        val account = accountIdB64() ?: return null
        val key = issuerKey(period) ?: return null
        val stored = prefs.getString("$account/$period", null)?.let { java.util.Base64.getDecoder().decode(it) }
        if (stored != null) return stored to key
        val seed = lineageSeed() ?: return null
        val secret = try {
            uniffi.lakko_ffi_uniffi.AnonCredentialSecret.fromLineageSeed(seed)
        } finally {
            seed.fill(0)
        }
        secret.use { s ->
            val token = bearer() ?: return null
            var response = issue(token, s.issuanceRequest(key))
            if (response.status.value == 409 && codeOf(response) == "commitment-not-registered") {
                val reg = client.put("$baseUrl/auth/anon-credential/commitment") {
                    header(HttpHeaders.Authorization, "Bearer $token")
                    contentType(ContentType.Application.Json)
                    setBody(buildJsonObject { put("registration_b64", b64u(s.registration())) }.toString())
                }
                if (reg.status.value != 204 && reg.status.value != 409) return null
                response = issue(token, s.issuanceRequest(key))
            }
            if (response.status.value !in 200..299) return null
            val obj = Json.parseToJsonElement(response.bodyAsText()) as? JsonObject ?: return null
            val responseB64 = obj["response_b64"]?.jsonPrimitive?.content ?: return null
            val credential = s.finalizeIssuance(key, unb64u(responseB64))
            prefs.edit().putString("$account/$period", java.util.Base64.getEncoder().encodeToString(credential)).apply()
            credential to key
        }
    }

    /** Present [credential] for one vote message on one post. */
    public fun present(
        credential: ByteArray,
        key: uniffi.lakko_ffi_uniffi.AnonIssuerPublicKey,
        channelId: ByteArray,
        postCursor: ULong,
        message: ByteArray,
    ): uniffi.lakko_ffi_uniffi.AnonPresentation? {
        val seed = lineageSeed() ?: return null
        return try {
            uniffi.lakko_ffi_uniffi.AnonCredentialSecret.fromLineageSeed(seed).use { s ->
                s.present(credential, key, channelId, postCursor, message)
            }
        } catch (cause: Throwable) {
            Log.w(TAG, "credential presentation refused (${cause.javaClass.simpleName})")
            null
        } finally {
            seed.fill(0)
        }
    }

    private suspend fun issue(token: String, request: ByteArray): HttpResponse =
        client.post("$baseUrl/auth/anon-credential") {
            header(HttpHeaders.Authorization, "Bearer $token")
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("request_b64", b64u(request)) }.toString())
        }

    /** The issuer key of [period], ALWAYS from the anonymous key route. */
    private suspend fun issuerKey(period: ULong): uniffi.lakko_ffi_uniffi.AnonIssuerPublicKey? = try {
        val r = client.get("$baseUrl/auth/anon-credential/key/$period")
        if (r.status.value !in 200..299) {
            null
        } else {
            val obj = Json.parseToJsonElement(r.bodyAsText()) as JsonObject
            val bytes = unb64u(obj["public_key_b64"]!!.jsonPrimitive.content)
            uniffi.lakko_ffi_uniffi.AnonIssuerPublicKey.decode(period, bytes)
        }
    } catch (cause: Throwable) {
        if (cause is kotlinx.coroutines.CancellationException) throw cause
        null
    }

    private suspend fun codeOf(r: HttpResponse): String? =
        runCatching { (Json.parseToJsonElement(r.bodyAsText()) as JsonObject)["code"]?.jsonPrimitive?.content }.getOrNull()
            ?: runCatching { (Json.parseToJsonElement(r.bodyAsText()) as JsonObject)["error"]?.jsonPrimitive?.content }.getOrNull()

    /**
     * ADR-0090 section 4.3 — vote (or, with no [labels], take the vote back)
     * on one channel poll post. Returns whether it was stored.
     */
    public suspend fun vote(
        channelIdHex: String,
        channelId: ByteArray,
        postCursor: ULong,
        labels: List<ByteArray>,
        nowMs: () -> Long = System::currentTimeMillis,
    ): Boolean {
        val period = uniffi.lakko_ffi_uniffi.anonCredPeriodAt((nowMs() / 1000).toULong())
        val (credential, key) = credentialFor(period) ?: return false
        val sorted = labels.sortedWith { a, b -> me.lakko.app.chat.compareBytes(a, b) }
        val action = if (sorted.isEmpty()) {
            uniffi.lakko_ffi_uniffi.ChannelPollVoteAction.DELETE
        } else {
            uniffi.lakko_ffi_uniffi.ChannelPollVoteAction.PUT
        }
        val channelsBase = ApiEndpoints.baseUrlFor(ApiService.CHANNELS)
        return castChannelVote(nowMs) { at ->
            val message = uniffi.lakko_ffi_uniffi.channelPollVoteMessage(action, at.toULong(), sorted)
            val p = present(credential, key, channelId, postCursor, message) ?: return@castChannelVote ChannelVoteAnswer.Refused(0, null)
            val tagHex = p.tag.joinToString("") { "%02x".format(it) }
            val body = buildJsonObject {
                put("presentation_b64", b64u(p.bytes))
                if (sorted.isNotEmpty()) putJsonArray("labels_hex") { sorted.forEach { add(it.joinToString("") { b -> "%02x".format(b) }) } }
                put("at_ms", at)
            }.toString()
            val r = client.request("$channelsBase/v1/channels/$channelIdHex/posts/$postCursor/votes/$tagHex") {
                method = if (sorted.isEmpty()) HttpMethod.Delete else HttpMethod.Put
                contentType(ContentType.Application.Json)
                setBody(body)
            }
            voteAnswerOf(r.status.value, runCatching { r.bodyAsText() }.getOrNull())
        }
    }

    public companion object {
        private const val TAG = "ChannelPollVoting"
        private const val PREFS = "lakko-channel-poll-credentials"

        @Volatile
        public var active: ChannelPollCredentials? = null

        /** Forget device / account wipe: drop [accountIdB64]'s credentials. */
        public fun deleteAccountRows(context: Context, accountIdB64: String) {
            val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            val edit = prefs.edit()
            for (k in prefs.all.keys) if (k.startsWith("$accountIdB64/")) edit.remove(k)
            edit.apply()
        }

        private fun b64u(bytes: ByteArray): String =
            java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
        private fun unb64u(s: String): ByteArray = java.util.Base64.getUrlDecoder().decode(s)
    }
}
