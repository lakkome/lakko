// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.get
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
import uniffi.lakko_ffi_uniffi.FfiCallJwtClaims
import uniffi.lakko_ffi_uniffi.FfiCallRole
import uniffi.lakko_ffi_uniffi.parseCallJwtUnverified
import uniffi.lakko_ffi_uniffi.verifyCallJwtWithJwks

/**
 * Client-side HTTP transport for the Phase 3b4 call-admission JWT
 * pipeline — Android mirror of the web `CallJwtClient`
 * (`apps/web/src/calls/call-jwt-client.ts`) and the iOS
 * `CallJwtClient.swift`.
 *
 *   * [mint] — `POST /call/jwt` with a bearer session token.
 *               Returns the JWT + parsed (UNVERIFIED) claims.
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
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a network. */
    engine: HttpClientEngine? = null,
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
        HttpClient(OkHttp) {
            install(ContentNegotiation) { json(json) }
        }
    }

    @Volatile
    private var sessionToken: String? = null

    /** Rebind the bearer token. `null` clears (sign-out). */
    public fun setSessionToken(token: String?) {
        sessionToken = token
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Mint
    // -----------------------------------------------------------------

    /**
     * `POST /call/jwt` — mints a JWT scoped to
     * `(account_id-from-bearer, callId, role)`. Returns the
     * raw JWS Compact representation along with the
     * **unverified** parsed claims for UI convenience.
     *
     * @throws CallJwtArgError when [callId] doesn't match
     *         `^[0-9a-f]{32}$` or no bearer token is set.
     * @throws CallJwtHttpError on a non-2xx response (401 on
     *         missing / bad bearer, 400 on the server's own
     *         `call_id` validation, 500 on a mint failure).
     * @throws CallJwtNetworkError on transport errors.
     */
    public suspend fun mint(
        callId: String,
        role: CallRole = CallRole.PARTICIPANT,
    ): MintCallJwtResult {
        require(CALL_ID_PATTERN.matches(callId)) {
            "call_id must be 32 lower-case hex chars, got ${callId.length} chars"
        }
        val token = sessionToken
            ?: throw CallJwtArgError("session token is not bound — call setSessionToken before mint()")
        val endpoint = "/call/jwt"
        val response: HttpResponse = try {
            client.post("$baseUrl$endpoint") {
                contentType(ContentType.Application.Json)
                headers { append(HttpHeaders.Authorization, "Bearer $token") }
                setBody(IssueRequest(callId = callId, role = role.wire))
            }
        } catch (cause: CallJwtArgError) {
            throw cause
        } catch (cause: Throwable) {
            throw CallJwtNetworkError(endpoint, cause)
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
        )
    }

    // -----------------------------------------------------------------
    // JWKS fetch
    // -----------------------------------------------------------------

    /**
     * `GET /.well-known/jwks.json` — fetches the auth service's
     * published JWK Set. The returned `ByteArray` is the raw
     * JSON document; pass it into [verifyAgainstJwks] or cache
     * it. Anonymous endpoint — no bearer required.
     */
    public suspend fun fetchJwks(): ByteArray {
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
        return response.readBytes()
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

    public companion object {
        /**
         * `10.0.2.2` is the canonical Android-emulator alias for
         * the host workstation. Matches the AuthClient default.
         */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8081"

        private val CALL_ID_PATTERN: Regex = Regex("^[0-9a-f]{32}$")
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
    val sub: String,
    val callId: String,
    val role: CallRole,
    val iat: ULong,
    val exp: ULong,
    val kid: String,
) {
    internal companion object {
        internal fun from(ffi: FfiCallJwtClaims): ParsedCallJwt = ParsedCallJwt(
            iss = ffi.iss,
            aud = ffi.aud,
            sub = ffi.sub,
            callId = ffi.callId,
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
)

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

/** `POST /call/jwt` request shape — snake_case keys match the
 *  server. Kept internal; callers use [CallJwtClient.mint]. */
@Serializable
private data class IssueRequest(
    @SerialName("call_id") val callId: String,
    val role: String,
)

/** `POST /call/jwt` response shape. */
@Serializable
private data class IssueResponse(
    val jwt: String,
    @SerialName("expires_at_unix_secs") val expiresAtUnixSecs: ULong,
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
