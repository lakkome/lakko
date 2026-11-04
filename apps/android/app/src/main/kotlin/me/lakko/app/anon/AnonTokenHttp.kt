// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import okhttp3.ConnectionPool
import okhttp3.CookieJar
import uniffi.lakko_ffi_uniffi.AnonTokenRequest
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.anonTokenDefaultWindowSecs
import uniffi.lakko_ffi_uniffi.anonTokenEpochAt
import uniffi.lakko_ffi_uniffi.anonTokenHeaderName
import uniffi.lakko_ffi_uniffi.anonTokenIsLive
import kotlin.coroutines.cancellation.CancellationException

/**
 * The HTTP engine every token route uses (ADR-0054 privacy rule): its OWN
 * OkHttp connection pool and no cookie jar, and no client built on it ever
 * attaches `Authorization`.
 *
 * Ktor's OkHttp engines are all built from one static prototype client, so
 * without an explicit [ConnectionPool] every engine in the app shares one pool
 * and a token request could ride the same HTTP/2 connection an authenticated
 * call just used, which would link the two. It still goes through
 * [AntiCensorshipEngine], so an armed tunnel carries it like everything else.
 *
 * Residual, not closed here: the TLS stack's session cache is process-wide,
 * so a resumed TLS session could still let the server associate the two
 * connections.
 */
public object AnonymousHttp {
    public val engine: HttpClientEngine by lazy {
        AntiCensorshipEngine(
            me.lakko.app.transport.BaseHttpEngine.create {
                config {
                    connectionPool(ConnectionPool())
                    cookieJar(CookieJar.NO_COOKIES)
                }
            },
        )
    }
}

/** The body code of a `401` token refusal, or `null` for any other answer. */
public suspend fun anonTokenRefusalCode(response: HttpResponse): String? {
    if (response.status.value != HTTP_UNAUTHORIZED) return null
    val code = errorCodeOf(runCatching { response.bodyAsText() }.getOrNull()) ?: return null
    return code.takeIf { it.startsWith("anon-token-") }
}

/** Send a token route with one retry on a token refusal
 *  ([ANON_TOKEN_RETRYABLE_REFUSALS]). [send] builds the request with the
 *  header it is handed and MUST NOT attach any session. */
public suspend fun AnonTokenSource.sendWithToken(
    context: AnonTokenContext,
    send: suspend (headerName: String, headerValue: String) -> HttpResponse,
): HttpResponse {
    val name = headerName
    return spendWithRetry(
        context = context,
        send = { token -> send(name, token.headerValue) },
        refusalCode = ::anonTokenRefusalCode,
    )
}

/** `{"error": "<code>", ...}` -> the code, or `null`. */
public fun errorCodeOf(body: String?): String? {
    if (body.isNullOrBlank()) return null
    return try {
        (lenientJson.parseToJsonElement(body) as? JsonObject)
            ?.get("error")?.jsonPrimitive?.contentOrNull
    } catch (_: Throwable) {
        null
    }
}

/** A numeric field of a JSON error body, or `null`. */
public fun longFieldOf(body: String?, field: String): Long? {
    if (body.isNullOrBlank()) return null
    return try {
        (lenientJson.parseToJsonElement(body) as? JsonObject)
            ?.get(field)?.jsonPrimitive?.longOrNull
    } catch (_: Throwable) {
        null
    }
}

private val lenientJson = Json { ignoreUnknownKeys = true }

private const val HTTP_UNAUTHORIZED: Int = 401

// -----------------------------------------------------------------------------
// The issuer over Ktor
// -----------------------------------------------------------------------------

@Serializable
@Suppress("ConstructorParameterNaming")
private data class IssueWireResponse(
    val context: String,
    val epoch: Long,
    val evaluations: List<String>,
    val proof: String,
    val remaining: Long? = null,
    val epoch_ends_at: Long? = null,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class KeyWireResponse(
    val context: String,
    val epoch: Long,
    val public_key: String,
)

/**
 * `POST /auth/tokens` (with the session) and
 * `GET /auth/tokens/key/{context}/{epoch}` (anonymous, cached for the 300 s
 * the route allows). The key a batch is verified against always comes from
 * the GET, never from the `public_key` of the issuance answer.
 */
public class KtorAnonTokenIssuer(
    /** The auth service base (`<api>/auth`). */
    private val baseUrl: String = ApiEndpoints.baseUrlFor(ApiService.AUTH),
    /** Engine for the issuance call (it carries the session). */
    engine: HttpClientEngine? = null,
    /** Engine for the anonymous key fetch. */
    anonEngine: HttpClientEngine? = null,
    private val nowMs: () -> Long = System::currentTimeMillis,
) : AnonTokenIssuerApi {

    private val sessionClient: HttpClient =
        HttpClient(engine ?: lakkoHttpEngine())
    private val anonClient: HttpClient = HttpClient(anonEngine ?: engine ?: AnonymousHttp.engine)

    private val keyCache = HashMap<Pair<AnonTokenContext, Long>, Pair<String, Long>>()

    override suspend fun issue(session: AnonSession, bodyJson: String): AnonIssueOutcome {
        val response: HttpResponse = try {
            sessionClient.post("$baseUrl/auth/tokens") {
                header(HttpHeaders.Authorization, "Bearer ${session.bearer}")
                contentType(ContentType.Application.Json)
                setBody(bodyJson)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            return AnonIssueOutcome.Failed(e)
        }
        val text = runCatching { response.bodyAsText() }.getOrNull()
        val status = response.status.value
        val code = errorCodeOf(text)
        return when {
            status in 200..299 -> try {
                val parsed = lenientJson.decodeFromString(IssueWireResponse.serializer(), text ?: "")
                AnonIssueOutcome.Issued(parsed.epoch, parsed.evaluations, parsed.proof)
            } catch (e: Throwable) {
                AnonIssueOutcome.Failed(e)
            }
            status == 400 && code == "epoch-not-issuable" -> {
                val current = longFieldOf(text, "current_epoch")
                if (current == null) {
                    AnonIssueOutcome.Failed(IllegalStateException("epoch-not-issuable without current_epoch"))
                } else {
                    AnonIssueOutcome.EpochNotIssuable(current)
                }
            }
            status == 429 && code == "anon-token-quota-exhausted" -> {
                val retry = longFieldOf(text, "retry_after_secs")
                    ?: response.headers[HttpHeaders.RetryAfter]?.trim()?.toLongOrNull()
                    ?: DEFAULT_QUOTA_RETRY_SECS
                AnonIssueOutcome.QuotaExhausted(retry.coerceAtLeast(1))
            }
            status == 503 && code == "anon-tokens-disabled" -> AnonIssueOutcome.Disabled
            else -> AnonIssueOutcome.Failed(
                IllegalStateException("POST /auth/tokens answered $status ${code ?: ""}".trim()),
            )
        }
    }

    override suspend fun publicKey(context: AnonTokenContext, epoch: Long): String? {
        val cacheKey = context to epoch
        synchronized(keyCache) {
            keyCache[cacheKey]?.let { (key, at) ->
                if (nowMs() - at < KEY_CACHE_MS) return key
            }
        }
        val response: HttpResponse = try {
            anonClient.get("$baseUrl/auth/tokens/key/${context.wire}/$epoch")
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            return null
        }
        if (response.status.value !in 200..299) return null
        val parsed = try {
            lenientJson.decodeFromString(KeyWireResponse.serializer(), response.bodyAsText())
        } catch (_: Throwable) {
            return null
        }
        if (parsed.context != context.wire || parsed.epoch != epoch) return null
        synchronized(keyCache) { keyCache[cacheKey] = parsed.public_key to nowMs() }
        return parsed.public_key
    }

    public companion object {
        /** The key route's `max-age`. */
        public const val KEY_CACHE_MS: Long = 300_000L

        /** A quota refusal with no stated wait. */
        private const val DEFAULT_QUOTA_RETRY_SECS: Long = 3600L
    }
}

// -----------------------------------------------------------------------------
// The native core
// -----------------------------------------------------------------------------

/** [AnonTokenCore] over the UniFFI bindings. */
public object UniffiAnonTokenCore : AnonTokenCore {
    override val headerName: String by lazy { anonTokenHeaderName() }

    override val defaultWindowSecs: Long by lazy { anonTokenDefaultWindowSecs().toLong() }

    override fun epochAt(unixSecs: Long, windowSecs: Long): Long =
        anonTokenEpochAt(unixSecs.toULong(), windowSecs.toULong()).toLong()

    override fun isLive(tokenEpoch: Long, currentEpoch: Long): Boolean =
        anonTokenIsLive(tokenEpoch.toULong(), currentEpoch.toULong())

    override fun begin(context: AnonTokenContext, epoch: Long, count: Int): AnonIssuance {
        val request = AnonTokenRequest(context.wire, epoch.toULong(), count.toUInt())
        return object : AnonIssuance {
            override fun issueBodyJson(): String = request.issueBodyJson()

            override fun finalize(
                evaluations: List<String>,
                proof: String,
                publicKey: String,
            ): List<PooledAnonToken> = try {
                request.finalize(evaluations, proof, publicKey).map {
                    PooledAnonToken(context, it.epoch.toLong(), it.headerValue)
                }
            } catch (e: FfiException) {
                val code = e.message?.substringBefore(':')?.trim().orEmpty().ifEmpty { "internal" }
                throw AnonFinalizeFailed(code, e)
            }

            override fun close() {
                request.close()
            }
        }
    }
}

// -----------------------------------------------------------------------------
// The process-wide pool
// -----------------------------------------------------------------------------

/**
 * The one pool the app spends from. `AuthRepository` installs the session
 * source and the on-disk store when it is built, releases the memory on
 * logout ([releaseSession], the account's tokens stay on disk) and the wipe
 * removes one account's tokens ([forgetAccount]); a process that has no repository (a cold FCM token refresh)
 * has no session, so its token routes fail and push registration retries at
 * the next launch.
 */
public object AnonTokens {
    @Volatile
    private var sessionSource: () -> AnonSession? = { null }

    @Volatile
    private var storeRef: AnonTokenStore = NoAnonTokenStore

    /** Forwards to whatever store is installed when it is used, so the store
     *  can be installed after the pool was built. */
    private val installedStore: AnonTokenStore = object : AnonTokenStore {
        override fun load(accountKey: String): PersistedAnonPool? = storeRef.load(accountKey)
        override fun save(pool: PersistedAnonPool) = storeRef.save(pool)
        override fun delete(accountKey: String) = storeRef.delete(accountKey)
    }

    @Volatile
    private var poolRef: AnonTokenPool? = null

    /** Accounts forgotten while no pool was built, handed to the pool when
     *  it is. Guarded by this object's monitor, as is building the pool. */
    private val forgottenBeforePool: HashSet<String> = HashSet()

    private fun pool(): AnonTokenPool =
        poolRef ?: synchronized(this) {
            poolRef ?: AnonTokenPool(
                core = UniffiAnonTokenCore,
                api = KtorAnonTokenIssuer(),
                session = { sessionSource() },
                scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
                store = installedStore,
                forgottenAccounts = forgottenBeforePool.toSet(),
                refillOnEmptyTake = me.lakko.app.BuildConfig.DEBUG,
            ).also {
                poolRef = it
                forgottenBeforePool.clear()
            }
        }

    /** Where the pool persists between launches (ADR-0054 S5). */
    public fun installStore(store: AnonTokenStore) {
        storeRef = store
    }

    /** Replace the session source. */
    public fun installSession(source: () -> AnonSession?) {
        sessionSource = source
    }

    /** The signed-in session changed: bind it and start its refill schedule
     *  (ADR-0078 section 8), so the pool fills ahead of any spend. Never
     *  throws; a pool that cannot be built (no native library in a host
     *  test) stays unbuilt. */
    public fun sessionBound() {
        runCatching { pool().bindSession() }
    }

    /** Log out: drop the tokens held in memory. The account's persisted
     *  tokens stay for its next sign-in (maintainer decision, 2026-10-01). */
    public fun releaseSession() {
        poolRef?.releaseSession()
    }

    /** The Forget device / Delete account wipe: remove [accountKey]'s tokens,
     *  held and persisted (a pool never built holds none in memory, but a
     *  previous launch may have left some on disk). Other accounts' stay. */
    public fun forgetAccount(accountKey: String) {
        // The session keys the pool with standard base64; the wipe may hold
        // another spelling of the same id.
        val key = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountKey)
        // Recorded for a pool built later too: a take that reads the session
        // between the wipe and the end of forgetDevice would otherwise build
        // one and write the account's file back.
        val pool = synchronized(this) {
            poolRef ?: run {
                forgottenBeforePool.add(key)
                null
            }
        }
        if (pool != null) pool.forgetAccount(key) else runCatching { storeRef.delete(key) }
    }

    /** [accountKey] landed on this device (a sign-in, a restore): a wipe of
     *  it earlier in this process no longer stops its pool being written.
     *  Called at every landing, next to the store gate's open. */
    public fun accountLanded(accountKey: String) {
        val key = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountKey)
        val pool = synchronized(this) {
            forgottenBeforePool.remove(key)
            poolRef
        }
        pool?.accountLanded(key)
    }

    /** What the spending clients use by default. Nothing native is touched
     *  until a token is actually needed. */
    public val shared: AnonTokenSource = object : AnonTokenSource {
        override val headerName: String get() = pool().headerName

        override suspend fun take(context: AnonTokenContext): PooledAnonToken = pool().take(context)

        override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken =
            pool().takeDetached(session, context)

        override fun releaseSession() {
            AnonTokens.releaseSession()
        }
    }
}
