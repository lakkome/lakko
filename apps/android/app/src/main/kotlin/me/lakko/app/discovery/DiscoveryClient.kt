// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.get
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.Json
import me.lakko.app.auth.PowChallengeResponse
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.auth.solvePow
import uniffi.lakko_ffi_uniffi.OprfBlinder

/**
 * Ktor-based client for `lakko-svc-discovery`.
 *
 * Mirror of `apps/web/src/discovery/client.ts`, scoped to the
 * 1.15e3 surface — OPRF + register + lookup + identity bundle
 * (upload + fetch). The KeyPackage endpoints land with the chat
 * surface (1.15f); the invite codec endpoints (upload / fetch /
 * consume / revoke) land with 1.15e6.
 *
 * Two layers, same shape as [me.lakko.app.auth.AuthClient]:
 *
 *   1. **Low-level methods** map the HTTP endpoints 1:1.
 *      Take a typed body, return a typed response (or `null` on
 *      the documented 404 path), do nothing else.
 *   2. **High-level [registerUsername] / [lookup] /
 *      [uploadIdentityBundle]** orchestrate the multi-round-trip
 *      flows: PoW₁ → OPRF blind → POST `/oprf` → finalize →
 *      PoW₂ → POST `/register` (or `/lookup`); and the simpler
 *      one-PoW identity-bundle upload.
 *
 * ## Why a separate PoW challenge?
 *
 * `services/discovery` keeps its own PoW challenge table; the
 * auth service's seeds are not accepted here. We call
 * `POST /pow/challenge` on the discovery base URL (not the auth
 * one) — the JSON shape happens to match, so we reuse
 * [PowChallengeResponse] from `me.lakko.app.auth.Wire`.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8084` — the canonical Android-emulator alias
 * for the host machine, port `8084` matching the `LAKKO_BIND` dev
 * setup for `lakko-svc-discovery`. Override via the constructor
 * for physical devices (LAN IP) and production (Cloudflare
 * tunnel URL).
 */
public class DiscoveryClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a
     *  network. */
    engine: HttpClientEngine? = null,
    /** Dispatcher for the PoW solver. Defaults to
     *  [Dispatchers.Default]; override to a `TestDispatcher`
     *  inside `runTest`. */
    private val powDispatcher: CoroutineDispatcher = Dispatchers.Default,
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
    // Low-level: 1:1 HTTP endpoints
    // -----------------------------------------------------------------

    /** `POST /pow/challenge`. */
    public suspend fun powChallenge(): PowChallengeResponse =
        doPost("/pow/challenge") { /* no body */ }

    /** `POST /discovery/oprf`. */
    public suspend fun oprfEvaluate(req: OprfRequest): OprfResponse =
        doPost("/discovery/oprf") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/register`. */
    public suspend fun register(req: RegisterRequest): RegisterResponse =
        doPost("/discovery/register") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `POST /discovery/lookup`. Returns the typed response on
     * 200, `null` on 404 (the typed not-found path), throws on
     * any other non-2xx.
     */
    public suspend fun lookupRaw(req: LookupRequest): LookupResponse? =
        doPostNullableOn404("/discovery/lookup") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/identity` — upsert the identity bundle. */
    public suspend fun identityUpload(req: IdentityUploadRequest): IdentityUploadResponse =
        doPost("/discovery/identity") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `GET /discovery/identity/{accountId_b64url}`. Returns the
     * bundle on 200, `null` on 404, throws on any other non-2xx.
     *
     * The path uses **base64url-without-padding** because axum
     * rejects `+` and `/` in path segments; the rest of the
     * wire format uses standard-alphabet base64.
     */
    public suspend fun identityFetch(accountId: ByteArray): IdentityFetchResponse? {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        val slug = base64UrlEncodeNoPad(accountId)
        val endpoint = "/discovery/identity/$slug"
        return getNullableOn404(endpoint)
    }

    /** `POST /discovery/keypackage` — upsert the user's MLS
     *  KeyPackage. */
    public suspend fun keypackageUpload(
        req: KeyPackageUploadRequest,
    ): KeyPackageUploadResponse =
        doPost("/discovery/keypackage") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `GET /discovery/keypackage/{accountId_b64url}`. Returns
     * the KeyPackage shape on 200, `null` on 404, throws on
     * any other non-2xx.
     *
     * Same base64url-no-pad path-slug shape as
     * [identityFetch] for the same reason (axum + `/` in path
     * segments).
     */
    public suspend fun keypackageFetch(accountId: ByteArray): KeyPackageFetchResponse? {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        val slug = base64UrlEncodeNoPad(accountId)
        val endpoint = "/discovery/keypackage/$slug"
        return getNullableOn404(endpoint)
    }

    // -----------------------------------------------------------------
    // High-level: full register / lookup / upload flows
    // -----------------------------------------------------------------

    /**
     * Bind `@username` to [accountId] server-side.
     *
     * Flow: PoW₁ → OPRF blind → `POST /discovery/oprf` → finalize
     * → PoW₂ → `POST /discovery/register`.
     *
     * Username normalisation is handled inside [OprfBlinder.blind]
     * (the UniFFI helper); the canonical form is surfaced back
     * via [RegisterResult.normalisedUsername] so the UI can show
     * what was actually claimed.
     *
     * Throws [DiscoveryHttpError] with `status == 409` on
     * username collision — usernames are first-come-first-served
     * and immutable in Phase 1.15.
     */
    public suspend fun registerUsername(
        accountId: ByteArray,
        username: String,
    ): RegisterResult {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        return OprfBlinder.blind(username).use { blinder ->
            // Read the normalised form **before** finalize — after
            // it the UniFFI sentinel kicks in (1.15e1's
            // DiscoveryAlreadyFinalized).
            val normalisedUsername = blinder.normalizedUsername()

            val challenge1 = powChallenge()
            val pow1 = solvePow(challenge1.seed, challenge1.effort_bits, powDispatcher)
            val oprfResp = oprfEvaluate(
                OprfRequest(pow = pow1, blinded = blinder.blindedB64()),
            )

            val oprfHashBytes = blinder.finalize(oprfResp.evaluated)
            val oprfHashB64 = base64Encode(oprfHashBytes)

            val challenge2 = powChallenge()
            val pow2 = solvePow(challenge2.seed, challenge2.effort_bits, powDispatcher)

            val resp = register(
                RegisterRequest(
                    pow = pow2,
                    account_id = base64Encode(accountId),
                    oprf_hash = oprfHashB64,
                ),
            )
            if (!resp.ok) {
                throw DiscoveryHttpError(
                    status = 200,
                    endpoint = "/discovery/register",
                    body = "server reported ok=false",
                )
            }
            RegisterResult(normalisedUsername = normalisedUsername)
        }
    }

    /**
     * Resolve `@username` to a raw 16-byte account_id, or `null`
     * when the username is not registered.
     *
     * Same flow as [registerUsername] modulo the second endpoint:
     * PoW₁ → OPRF blind → `POST /discovery/oprf` → finalize →
     * PoW₂ → `POST /discovery/lookup`.
     */
    public suspend fun lookup(username: String): LookupResult {
        return OprfBlinder.blind(username).use { blinder ->
            val normalisedUsername = blinder.normalizedUsername()

            val challenge1 = powChallenge()
            val pow1 = solvePow(challenge1.seed, challenge1.effort_bits, powDispatcher)
            val oprfResp = oprfEvaluate(
                OprfRequest(pow = pow1, blinded = blinder.blindedB64()),
            )

            val oprfHashBytes = blinder.finalize(oprfResp.evaluated)
            val oprfHashB64 = base64Encode(oprfHashBytes)

            val challenge2 = powChallenge()
            val pow2 = solvePow(challenge2.seed, challenge2.effort_bits, powDispatcher)

            val resp = lookupRaw(LookupRequest(pow = pow2, oprf_hash = oprfHashB64))
            if (resp == null) {
                LookupResult(normalisedUsername = normalisedUsername, account = null)
            } else {
                LookupResult(
                    normalisedUsername = normalisedUsername,
                    account = base64Decode(resp.account_id),
                )
            }
        }
    }

    /**
     * Upload (or replace) the user's MLS KeyPackage. Drives one
     * PoW solve + `POST /discovery/keypackage`.
     *
     * Unlike the identity bundle, KeyPackages are **not**
     * deterministic — `Identity.createKeyPackage()` generates
     * fresh randomness on every call (RFC 9420 forward-secrecy
     * property). Calling this on every login refreshes the
     * `init_key` on the server, so a fresh sender starting a
     * new 1×1 group gets bytes the recipient hasn't already
     * used.
     *
     * The Phase-1 directory only stores ONE KeyPackage per
     * account at a time (consume-on-use is post-1.15 work), so
     * a sender racing two sessions can in theory seed two
     * groups with the same `init_key`. Documented MVP trade-off
     * matching `apps/web`'s posture.
     */
    public suspend fun uploadKeyPackage(
        accountId: ByteArray,
        keyPackageBytes: ByteArray,
    ) {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (keyPackageBytes.isEmpty()) {
            throw DiscoveryClientArgError("keyPackageBytes must be non-empty")
        }
        if (keyPackageBytes.size > MAX_KEYPACKAGE_BYTES) {
            // Hard ceiling mirrors `MAX_KEYPACKAGE_BYTES = 2048`
            // on the server. Surfacing here saves a round-trip
            // on a misbehaving caller.
            throw DiscoveryClientArgError(
                "keyPackageBytes exceeds the $MAX_KEYPACKAGE_BYTES-byte ceiling " +
                    "(got ${keyPackageBytes.size} bytes)",
            )
        }

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val resp = keypackageUpload(
            KeyPackageUploadRequest(
                pow = pow,
                account_id = base64Encode(accountId),
                keypackage = base64Encode(keyPackageBytes),
            ),
        )
        if (!resp.ok) {
            throw DiscoveryHttpError(
                status = 200,
                endpoint = "/discovery/keypackage",
                body = "server reported ok=false",
            )
        }
    }

    /**
     * Resolve `accountId → KeyPackage bytes` (TLS-encoded MLS).
     * Returns `null` when the recipient hasn't published one
     * yet.
     *
     * Convenience wrapper around [keypackageFetch] that decodes
     * the base64 wire field to a [ByteArray] so the caller can
     * hand it straight to `Group.addMemberFromWire(...)`.
     */
    public suspend fun fetchKeyPackage(accountId: ByteArray): ByteArray? {
        val raw = keypackageFetch(accountId) ?: return null
        return base64Decode(raw.keypackage)
    }

    /**
     * Upload (or replace) the identity bundle for [accountId].
     * Drives one PoW solve + `POST /discovery/identity`.
     *
     * Idempotent — the server upserts via
     * `INSERT … ON DUPLICATE KEY UPDATE`. Calling this on every
     * login refreshes the bundle, important for the deterministic-
     * derivation property: the same password produces the same
     * Ed25519 + X25519 public keys, so the server's row never
     * changes content but the upsert path stays uniform.
     */
    public suspend fun uploadIdentityBundle(
        accountId: ByteArray,
        signaturePubkey: ByteArray,
        x25519Pubkey: ByteArray,
    ) {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (signaturePubkey.size != PUBKEY_LEN) {
            throw DiscoveryClientArgError(
                "signaturePubkey must be $PUBKEY_LEN bytes (Ed25519)",
            )
        }
        if (x25519Pubkey.size != PUBKEY_LEN) {
            throw DiscoveryClientArgError(
                "x25519Pubkey must be $PUBKEY_LEN bytes (HPKE init_key)",
            )
        }

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val resp = identityUpload(
            IdentityUploadRequest(
                pow = pow,
                account_id = base64Encode(accountId),
                signature_pubkey = base64Encode(signaturePubkey),
                x25519_pubkey = base64Encode(x25519Pubkey),
            ),
        )
        if (!resp.ok) {
            throw DiscoveryHttpError(
                status = 200,
                endpoint = "/discovery/identity",
                body = "server reported ok=false",
            )
        }
    }

    // -----------------------------------------------------------------
    // Internal HTTP helpers
    // -----------------------------------------------------------------

    /**
     * One-shot POST. Reified `Res` so kotlinx-serialization can
     * decode the response body without runtime reflection. The
     * lambda configures method-specific bits (body, content-type).
     */
    private suspend inline fun <reified Res : Any> doPost(
        endpoint: String,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.post(url) { configure() }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError(endpoint, cause)
        }
        if (!response.status.isSuccess()) {
            throw DiscoveryHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        return response.body()
    }

    /**
     * Variant of [doPost] that folds a documented 404 into a
     * `null` return. Used by `/discovery/lookup` (unknown
     * username) and the `GET /discovery/identity/{...}` path
     * (no bundle). 404 is a typed not-found, not an error — see
     * the docstrings on the relevant low-level methods.
     */
    private suspend inline fun <reified Res : Any> doPostNullableOn404(
        endpoint: String,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res? {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.post(url) { configure() }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError(endpoint, cause)
        }
        if (response.status == HttpStatusCode.NotFound) return null
        if (!response.status.isSuccess()) {
            throw DiscoveryHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        return response.body()
    }

    /**
     * `GET` variant of [doPostNullableOn404]. Same 404→null
     * fold, no request body — used by the
     * `/discovery/identity/{slug}` and `/discovery/keypackage/{slug}`
     * fetch paths. Pulled out of the inline endpoint methods
     * so the error / 404 handling stays uniform across both
     * fetches.
     */
    private suspend inline fun <reified Res : Any> getNullableOn404(
        endpoint: String,
    ): Res? {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.get(url)
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (GET)", cause)
        }
        if (response.status == HttpStatusCode.NotFound) return null
        if (!response.status.isSuccess()) {
            throw DiscoveryHttpError(
                status = response.status.value,
                endpoint = "$endpoint (GET)",
                body = response.safeReadBody(),
            )
        }
        return response.body()
    }

    public companion object {
        /**
         * Default discovery service URL — the Android emulator
         * loopback alias for the host machine, port `8084`
         * matching the `LAKKO_BIND` dev setup for
         * `lakko-svc-discovery`.
         */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8084"

        /** Raw 16-byte UUIDv7. */
        private const val ACCOUNT_ID_LEN: Int = 16

        /** Ed25519 + X25519 both use 32-byte public keys. */
        private const val PUBKEY_LEN: Int = 32

        /** Mirror of `services/discovery`'s `MAX_KEYPACKAGE_BYTES`.
         *  Surfacing the ceiling here saves a round-trip on a
         *  misbehaving caller. */
        private const val MAX_KEYPACKAGE_BYTES: Int = 2048
    }
}

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}

// =============================================================================
// Result types
// =============================================================================

/** Result of [DiscoveryClient.registerUsername]. */
public data class RegisterResult(
    /** The canonical form actually claimed (after NFKC + lowercase
     *  + trim + leading-`@` strip). Surface to the UI so the user
     *  sees what handle is now bound to their account. */
    val normalisedUsername: String,
)

/** Result of [DiscoveryClient.lookup]. */
public data class LookupResult(
    val normalisedUsername: String,
    /** Raw 16-byte account_id, or `null` if the username is not
     *  registered. */
    val account: ByteArray?,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is LookupResult) return false
        return normalisedUsername == other.normalisedUsername &&
            (account?.contentEquals(other.account) ?: (other.account == null))
    }

    override fun hashCode(): Int {
        var result = normalisedUsername.hashCode()
        result = 31 * result + (account?.contentHashCode() ?: 0)
        return result
    }
}

// =============================================================================
// Errors
// =============================================================================

/** Programmer error — empty / invalid inputs caught before any
 *  network round-trip. Distinct from HTTP errors so the UI can
 *  surface a form-validation message. */
public class DiscoveryClientArgError(message: String) : IllegalArgumentException(message)

/** Server returned a non-2xx status. Carries the original status
 *  + endpoint so the UI can branch on (e.g.) 409 = taken,
 *  404 = unknown. */
public class DiscoveryHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : RuntimeException("discovery $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). Distinct from
 *  [DiscoveryHttpError] so the UI can suggest "check connection". */
public class DiscoveryNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("discovery $endpoint network error: $cause", cause)
