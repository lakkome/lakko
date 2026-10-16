// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.headers
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.client.statement.readBytes
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import java.security.SecureRandom
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.sendWithToken
import kotlin.coroutines.cancellation.CancellationException
import uniffi.lakko_ffi_uniffi.FfiCallJwtClaims
import uniffi.lakko_ffi_uniffi.FfiCallRole
import uniffi.lakko_ffi_uniffi.parseCallJwtUnverified
import uniffi.lakko_ffi_uniffi.sfuRoomForCall
import uniffi.lakko_ffi_uniffi.verifyCallJwtWithJwks

/**
 * Client-side HTTP transport for the Phase 3b4 call-admission JWT
 * pipeline — Android mirror of the web `CallJwtClient`
 * (`apps/web/src/calls/call-jwt-client.ts`) and the iOS
 * `CallJwtClient.swift`.
 *
 *   * [mint] — `POST /call/jwt`, paid for by one anonymous `call-join`
 *               token (ADR-0078 section 2): no session, no account. The
 *               token names a room (`sfuRoomForCall(callId)`) and a random
 *               peer id; the answer carries a TURN credential beside it.
 *   * [fetchJwks] — `GET /.well-known/jwks.json`. Anonymous;
 *                    caller caches.
 *   * [parseUnverified] — Wraps the UniFFI
 *                         `parseCallJwtUnverified` free function.
 *   * [verifyAgainstJwks] — Defensive self-verify via UniFFI
 *                            `verifyCallJwtWithJwks`.
 *
 * The crypto + wire-format primitives live in `lakko-call-jwt`
 * (Rust) and are surfaced through UniFFI as the
 * `parseCallJwtUnverified` + `verifyCallJwtWithJwks` free
 * functions plus the `FfiCallJwtClaims` Record + `FfiCallRole`
 * Enum. This file is the **HTTP transport** layer on top of them.
 *
 * See `docs/security/calls.md` C-15..C-23 for the invariants the
 * underlying primitives preserve.
 */
public class CallJwtClient(
    /** Base URL. No trailing slash. Production: the auth-service
     *  endpoint behind the Cloudflare tunnel. Emulator dev:
     *  `http://10.0.2.2:8081` to reach the dev workstation. */
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: [AnonymousHttp.engine] (its own pool, no
     *  cookies: the mint is anonymous). Tests: `MockEngine` for
     *  deterministic responses without a network. */
    engine: HttpClientEngine? = null,
    /** Time-to-live for the in-memory JWKS cache, in
     *  milliseconds. Defaults to 24 h per ADR-0008. Passing
     *  `0` disables caching — every call fetches fresh.
     *  Negative values are clamped to 0. */
    public val jwksTtlMs: Long = DEFAULT_JWKS_TTL_MS,
    /** Where the mint draws its `call-join` token. */
    private val anonTokens: AnonTokenSource = AnonTokens.shared,
) : AutoCloseable {

    /**
     * `explicitNulls = false` so an omitted optional field doesn't
     * land on the wire; mirror of the AuthClient JSON config.
     */
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

    /** JWKS cache — guarded by the same `@Volatile` lock to
     *  keep the read-then-write window short. Reads return
     *  the cached value if `Date.now()` is within
     *  [jwksTtlMs] of [JwksCacheEntry.fetchedAtMs]. */
    @Volatile
    private var jwksCache: JwksCacheEntry? = null

    /** Drop the cached JWKS doc. Call after a verify
     *  failure that suggests a key rotation has happened —
     *  the convenience [verifyJwt] does this for you on
     *  `no JWK matches header.kid` errors. */
    public fun clearJwksCache() {
        jwksCache = null
    }

    private fun cachedJwksIfFresh(): ByteArray? {
        val entry = jwksCache ?: return null
        val ttl = maxOf(jwksTtlMs, 0L)
        val ageMs = System.currentTimeMillis() - entry.fetchedAtMs
        return if (ageMs < ttl) entry.bytes else null
    }

    private fun storeJwks(bytes: ByteArray) {
        jwksCache = JwksCacheEntry(bytes, System.currentTimeMillis())
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Mint
    // -----------------------------------------------------------------

    /** [mint] for the room of the 16-byte [callId]. */
    public suspend fun mint(
        callId: ByteArray,
        role: CallRole = CallRole.PARTICIPANT,
    ): MintCallJwtResult {
        if (callId.size != 16) throw CallJwtArgError("call id must be 16 bytes, got ${callId.size}")
        val room = try {
            sfuRoomForCall(callId)
        } catch (cause: Throwable) {
            throw CallJwtArgError("call id refused: ${cause.message}")
        }
        return mintForRoom(room, role)
    }

    /**
     * `POST /call/jwt` for [room] (64 lower-case hex), paid for by one
     * `call-join` token, never a session. The answer is the call token (no
     * account, no call id in it) and, when the deployment has a relay, a TURN
     * credential whose username tag is random.
     *
     * @throws CallJwtArgError when [room] is not 64 lower-case hex.
     * @throws me.lakko.app.anon.AnonTokenException when no token can be had.
     * @throws CallJwtHttpError on any other non-2xx (`429
     *         room-admission-limit` included).
     * @throws CallJwtNetworkError on transport errors.
     */
    public suspend fun mintForRoom(
        room: String,
        role: CallRole = CallRole.PARTICIPANT,
    ): MintCallJwtResult {
        if (!ROOM_PATTERN.matches(room)) {
            throw CallJwtArgError("room must be 64 lower-case hex chars, got ${room.length} chars")
        }
        val endpoint = "/call/jwt"
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CALL_JOIN) { name, value ->
            try {
                client.post("$baseUrl$endpoint") {
                    header(name, value)
                    contentType(ContentType.Application.Json)
                    setBody(IssueRequest(room = room, role = role.wire))
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                throw CallJwtNetworkError(endpoint, cause)
            }
        }
        if (!response.status.isSuccess()) {
            throw CallJwtHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        val body: IssueResponse = response.body()
        val claims = parseUnverified(body.jwt)
        return MintCallJwtResult(
            jwt = body.jwt,
            expiresAtUnixSecs = body.expiresAtUnixSecs,
            claims = claims,
            turn = body.turn?.let(TurnCredential::fromWire),
        )
    }

    // -----------------------------------------------------------------
    // JWKS fetch
    // -----------------------------------------------------------------

    /**
     * `GET /.well-known/jwks.json` — fetches the auth service's
     * published JWK Set. The returned `ByteArray` is the raw
     * JSON document; pass it into [verifyAgainstJwks] or hold
     * it. Anonymous endpoint — no bearer required.
     *
     * **Caches in-memory for [jwksTtlMs] milliseconds**
     * (default 24 h per ADR-0008). Pass `forceRefresh = true`
     * to bypass the cache; on a `kid` mismatch during verify
     * the convenience [verifyJwt] does this for you. Direct
     * invalidation via [clearJwksCache].
     */
    public suspend fun fetchJwks(forceRefresh: Boolean = false): ByteArray {
        if (!forceRefresh) {
            cachedJwksIfFresh()?.let { return it }
        }
        val endpoint = "/.well-known/jwks.json"
        val response: HttpResponse = try {
            client.get("$baseUrl$endpoint") {
                headers { append(HttpHeaders.Accept, "application/json") }
            }
        } catch (cause: Throwable) {
            throw CallJwtNetworkError(endpoint, cause)
        }
        if (!response.status.isSuccess()) {
            throw CallJwtHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        val bytes = response.readBytes()
        storeJwks(bytes)
        return bytes
    }

    // -----------------------------------------------------------------
    // Parse / Verify
    // -----------------------------------------------------------------

    /**
     * Parse the JWT WITHOUT verifying the signature. Useful for
     * UI surfaces reading role / expiry off a JWT the caller
     * already has. The result should NOT drive any authorisation
     * decision; the SFU does the load-bearing verification.
     */
    public fun parseUnverified(jwt: String): ParsedCallJwt {
        val claims: FfiCallJwtClaims = try {
            parseCallJwtUnverified(jwt)
        } catch (cause: Throwable) {
            throw CallJwtVerifyError("parseCallJwtUnverified", cause)
        }
        return ParsedCallJwt.from(claims)
    }

    /**
     * Verify a JWT against a cached JWKS doc. Returns the
     * cryptographically-validated claim set. [nowUnixSecs]
     * defaults to the current wall clock; tests inject a fixed
     * value to drive the `exp` check.
     */
    public fun verifyAgainstJwks(
        jwt: String,
        jwksDocBytes: ByteArray,
        nowUnixSecs: ULong? = null,
    ): ParsedCallJwt {
        val now = nowUnixSecs ?: (System.currentTimeMillis() / 1000L).toULong()
        val claims: FfiCallJwtClaims = try {
            verifyCallJwtWithJwks(
                jwksDocBytes = jwksDocBytes,
                jwt = jwt,
                nowUnixSecs = now,
            )
        } catch (cause: Throwable) {
            throw CallJwtVerifyError("verifyCallJwtWithJwks", cause)
        }
        return ParsedCallJwt.from(claims)
    }

    /**
     * Convenience: fetch JWKS (cache-aware) + verify in one
     * call. On a `no JWK matches header.kid` failure — the
     * canonical key-rotation signal — invalidates the
     * cache, refetches once, retries the verify. Any second
     * failure surfaces verbatim.
     */
    public suspend fun verifyJwt(
        jwt: String,
        nowUnixSecs: ULong? = null,
    ): ParsedCallJwt {
        val cached = fetchJwks()
        try {
            return verifyAgainstJwks(jwt = jwt, jwksDocBytes = cached, nowUnixSecs = nowUnixSecs)
        } catch (firstErr: CallJwtVerifyError) {
            if (!isKidMismatch(firstErr)) {
                throw firstErr
            }
            clearJwksCache()
            val fresh = fetchJwks(forceRefresh = true)
            return verifyAgainstJwks(jwt = jwt, jwksDocBytes = fresh, nowUnixSecs = nowUnixSecs)
        }
    }

    private fun isKidMismatch(err: CallJwtVerifyError): Boolean {
        val message = err.cause?.message ?: err.message ?: return false
        return message.contains("no JWK matches header.kid") || message.contains("kid")
    }

    public companion object {
        /**
         * The auth service base, same as the AuthClient default:
         * `<LAKKO_API_BASE_URL>/auth` ([ApiEndpoints]), or the
         * emulator alias `http://10.0.2.2:8081` without one.
         */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.AUTH)

        /** Default JWKS cache TTL — 24 h per ADR-0008. */
        public const val DEFAULT_JWKS_TTL_MS: Long = 24L * 60L * 60L * 1000L

        private val ROOM_PATTERN: Regex = Regex("^[0-9a-f]{64}$")
        private val secureRandom: SecureRandom = SecureRandom()

        /**
         * Mint a 16-byte random `callId` and render it as 32
         * lower-case hex chars — the shape the server expects.
         */
        public fun mintCallId(): String {
            val bytes = ByteArray(16)
            secureRandom.nextBytes(bytes)
            return bytes.joinToString("") { "%02x".format(it) }
        }
    }
}

// -----------------------------------------------------------------
// Public types
// -----------------------------------------------------------------

/** Participation role baked into the minted JWT. Mirror of the
 *  Rust `CallRole` enum; the wire form is lower-case. */
public enum class CallRole(internal val wire: String) {
    PARTICIPANT("participant"),
    PUBLISHER("publisher"),
    SUBSCRIBER("subscriber");

    internal companion object {
        internal fun fromFfi(ffi: FfiCallRole): CallRole = when (ffi) {
            FfiCallRole.PARTICIPANT -> PARTICIPANT
            FfiCallRole.PUBLISHER -> PUBLISHER
            FfiCallRole.SUBSCRIBER -> SUBSCRIBER
        }
    }
}

/** Flat claim set returned by [CallJwtClient.parseUnverified] and
 *  [CallJwtClient.verifyAgainstJwks]. */
public data class ParsedCallJwt(
    val iss: String,
    val aud: String,
    /** The room, 64 lower-case hex (ADR-0078 section 2: no account and no
     *  call id in the token). */
    val room: String,
    /** The SFU peer id, 32 lower-case hex, random per token. */
    val peerId: String,
    val role: CallRole,
    val iat: ULong,
    val exp: ULong,
    val kid: String,
) {
    internal companion object {
        internal fun from(ffi: FfiCallJwtClaims): ParsedCallJwt = ParsedCallJwt(
            iss = ffi.iss,
            aud = ffi.aud,
            room = ffi.room,
            peerId = ffi.peerId,
            role = CallRole.fromFfi(ffi.role),
            iat = ffi.iat,
            exp = ffi.exp,
            kid = ffi.kid,
        )
    }
}

/** Result of [CallJwtClient.mint]. The `claims` field is the
 *  **unverified** parse of the JWT body — fine for UI surfaces;
 *  production callers that need a verifying parse should call
 *  [CallJwtClient.verifyAgainstJwks] and read THAT result. */
public data class MintCallJwtResult(
    val jwt: String,
    val expiresAtUnixSecs: ULong,
    val claims: ParsedCallJwt,
    /** The TURN credential minted beside the token, or `null` when the
     *  deployment has no relay (the call proceeds on STUN alone). */
    val turn: TurnCredential? = null,
)

/** A TURN credential in the coturn REST shape (`services/auth/src/turn.rs`):
 *  the username's tag is random per credential (ADR-0078 section 2). */
public data class TurnCredential(
    val urls: List<String>,
    val username: String,
    val password: String,
) {
    internal companion object {
        /** `null` for a credential with no URL, username or password. */
        internal fun fromWire(wire: TurnCredentialWire): TurnCredential? {
            val urls = wire.urls.filter { it.isNotEmpty() }
            if (urls.isEmpty() || wire.username.isEmpty() || wire.password.isEmpty()) return null
            return TurnCredential(urls, wire.username, wire.password)
        }
    }
}

// -----------------------------------------------------------------
// Typed errors
// -----------------------------------------------------------------

/** Caller misuse — bad arg shape before any I/O happens. */
public class CallJwtArgError(message: String) : Exception(message)

/** Server returned a non-2xx response. */
public class CallJwtHttpError(
    public val status: Int,
    public val endpoint: String,
    body: String,
) : Exception("$endpoint failed with HTTP $status${if (body.isNotEmpty()) ": $body" else ""}")

/** Network transport failure — DNS, TLS, abort, etc. */
public class CallJwtNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : Exception("$endpoint network error: ${cause.message ?: cause::class.simpleName}", cause)

/** UniFFI verify path rejected. */
public class CallJwtVerifyError(
    public val reason: String,
    cause: Throwable,
) : Exception("call-JWT verify failed: $reason: ${cause.message ?: cause::class.simpleName}", cause)

// -----------------------------------------------------------------
// Wire shape (private)
// -----------------------------------------------------------------

/** JWKS in-memory cache entry. Lifetime is the
 *  [CallJwtClient] instance. */
private data class JwksCacheEntry(
    val bytes: ByteArray,
    val fetchedAtMs: Long,
)

/** `POST /call/jwt` request shape — snake_case keys match the
 *  server. Kept internal; callers use [CallJwtClient.mint]. */
@Serializable
private data class IssueRequest(
    val room: String,
    val role: String,
)

/** `POST /call/jwt` response shape. */
@Serializable
private data class IssueResponse(
    val jwt: String,
    @SerialName("expires_at_unix_secs") val expiresAtUnixSecs: ULong,
    val turn: TurnCredentialWire? = null,
)

/** The `turn` object of a `POST /call/jwt` answer. */
@Serializable
internal data class TurnCredentialWire(
    val urls: List<String> = emptyList(),
    val username: String = "",
    val password: String = "",
)

// -----------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}
