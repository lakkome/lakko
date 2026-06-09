// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
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
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.solvePow
import uniffi.lakko_ffi_uniffi.OprfBlinder
import uniffi.lakko_ffi_uniffi.computeJoinerInviteTag
import uniffi.lakko_ffi_uniffi.signCallInviteRevocation
import uniffi.lakko_ffi_uniffi.signInviteRevocation
import uniffi.lakko_ffi_uniffi.signOwnerListQuery

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

    /**
     * Phase 1.5b: bearer token threaded into the
     * `Authorization: Bearer <token>` header on every protected
     * `lakko-svc-discovery` write. `null` means "anonymous";
     * unauthenticated paths (`/pow/challenge`, `/discovery/oprf`,
     * `/discovery/lookup`, all GETs, invite consume) still work.
     *
     * Volatile so a `setSessionToken` from one coroutine is
     * immediately visible to a concurrent request on another.
     * `URLSession`-equivalent in Ktor is thread-safe; the only
     * thing we need is field-level visibility.
     */
    @Volatile
    public var sessionToken: String? = null
        private set

    /** Update the bearer token in-place. Mirror of the
     *  iOS `DiscoveryClient.setSessionToken(_:)` shape. */
    public fun setSessionToken(token: String?) {
        sessionToken = token
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

    /** `POST /discovery/invite` — upload a verified
     *  `InviteCapability` blob, get the content-addressed
     *  `blob_id` back (Phase 1.18d1). */
    public suspend fun inviteUpload(req: InviteUploadRequest): InviteUploadResponse =
        doPost("/discovery/invite") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `GET /discovery/invite/{blob_id_b64url}`. Returns the
     * capability wire shape on 200, `null` on 404 (server folds
     * expired blobs into the same status), throws on any other
     * non-2xx.
     */
    public suspend fun inviteFetch(blobId: ByteArray): InviteFetchResponse? {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        val slug = base64UrlEncodeNoPad(blobId)
        val endpoint = "/discovery/invite/$slug"
        return getNullableOn404(endpoint)
    }

    /**
     * `POST /discovery/invite/{blob_id_b64url}/consume` —
     * atomic `max_uses` enforcement + per-joiner dedup
     * (Phase 1.18d2a + 1.18d3b). The high-level [consumeInvite]
     * wrapper drives the PoW solve + the local
     * [computeJoinerInviteTag] call + folds the 200 / 404 / 410
     * status trifecta into a discriminated [ConsumeInviteResult].
     *
     * Surface this low-level method when tests need to inject
     * a synthetic body; production paths use [consumeInvite].
     */
    public suspend fun inviteConsume(
        blobIdSlug: String,
        req: InviteConsumeRequest,
    ): InviteConsumeResponse =
        doPost("/discovery/invite/$blobIdSlug/consume") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/call-invite` — upload a verified
     *  `CallInviteCapability` blob, get the content-addressed
     *  `blob_id` back (Phase 3c3-server). Sibling of
     *  [inviteUpload]; wire shape is shared. The high-level
     *  [uploadCallInvite] wrapper drives the PoW solve. */
    public suspend fun callInviteUpload(req: InviteUploadRequest): InviteUploadResponse =
        doPost("/discovery/call-invite") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `GET /discovery/call-invite/{blob_id_b64url}`. Returns
     * the capability wire shape on 200, `null` on 404 (server
     * folds expired blobs into the same status), throws on
     * any other non-2xx.
     */
    public suspend fun callInviteFetch(blobId: ByteArray): InviteFetchResponse? {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        val slug = base64UrlEncodeNoPad(blobId)
        val endpoint = "/discovery/call-invite/$slug"
        return getNullableOn404(endpoint)
    }

    /**
     * `POST /discovery/call-invite/{blob_id_b64url}/consume` —
     * atomic `max_uses` enforcement + per-joiner dedup against
     * the call-invite ledger (Phase 3c3-multi-use). The high-
     * level [consumeCallInvite] wrapper drives the PoW solve +
     * the local `computeJoinerInviteTag` call + folds the 200 /
     * 404 / 410 status trifecta into a discriminated
     * [ConsumeInviteResult].
     */
    public suspend fun callInviteConsume(
        blobIdSlug: String,
        req: InviteConsumeRequest,
    ): InviteConsumeResponse =
        doPost("/discovery/call-invite/$blobIdSlug/consume") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `DELETE /discovery/invite/{blob_id_b64url}` — owner-
     * signed revocation. The body carries `signed_at_ms` + the
     * Ed25519 signature; the server recovers the inviter's
     * pubkey from the stored capability bytes and verifies
     * against the canonical revocation layout.
     *
     * 204 on success → returns; 404 → throws
     * [DiscoveryHttpError] which the high-level [revokeInvite]
     * wrapper folds into a discriminated `not-found-or-not-
     * owner` result. Other non-2xx still throw.
     */
    public suspend fun inviteRevoke(blobIdSlug: String, req: InviteRevokeRequest) {
        val endpoint = "/discovery/invite/$blobIdSlug"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.delete(url) {
                sessionToken?.let {
                    header(io.ktor.http.HttpHeaders.Authorization, "Bearer $it")
                }
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (DELETE)", cause)
        }
        if (response.status == HttpStatusCode.NoContent) return
        // 200 OK is also acceptable — some axum versions don't
        // emit 204 cleanly through `setBody`. Treat any 2xx as
        // success.
        if (response.status.isSuccess()) return
        throw DiscoveryHttpError(
            status = response.status.value,
            endpoint = "$endpoint (DELETE)",
            body = response.safeReadBody(),
        )
    }

    /** Wire-level `DELETE /discovery/call-invite/{blob_id}` —
     *  owner-signed revocation of a [CallInviteCapability] blob
     *  (Phase 3c3-revoke). Sibling of [inviteRevoke]; the wire
     *  body shape is identical ([InviteRevokeRequest]) but the
     *  signature is minted under a distinct domain separator
     *  (`lakko-call-invite-revoke/v1`) so a captured contact-
     *  invite revoke cannot be replayed here (and vice versa).
     *
     *  204 on success → returns; 404 → throws
     *  [DiscoveryHttpError] which the high-level
     *  [revokeCallInvite] wrapper folds into a discriminated
     *  `not-found-or-not-owner` result. */
    public suspend fun callInviteRevoke(blobIdSlug: String, req: InviteRevokeRequest) {
        val endpoint = "/discovery/call-invite/$blobIdSlug"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.delete(url) {
                sessionToken?.let {
                    header(io.ktor.http.HttpHeaders.Authorization, "Bearer $it")
                }
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (DELETE)", cause)
        }
        if (response.status == HttpStatusCode.NoContent) return
        if (response.status.isSuccess()) return
        throw DiscoveryHttpError(
            status = response.status.value,
            endpoint = "$endpoint (DELETE)",
            body = response.safeReadBody(),
        )
    }

    /** Wire-level `POST /discovery/invite/by-owner` (Phase
     *  1.18d2b-list). Returns the wire response on 200, `null`
     *  on 404, throws [DiscoveryHttpError] on other non-2xx.
     *  Session-token middleware is the DoS gate; the body
     *  signature is the actual ownership proof. The high-level
     *  [listInvitesByOwner] wrapper builds the signed body. */
    public suspend fun inviteOwnerList(req: InviteOwnerListRequest): InviteOwnerListResponse? {
        val endpoint = "/discovery/invite/by-owner"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.post(url) {
                sessionToken?.let {
                    header(io.ktor.http.HttpHeaders.Authorization, "Bearer $it")
                }
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (POST)", cause)
        }
        if (response.status == HttpStatusCode.NotFound) return null
        if (!response.status.isSuccess()) {
            throw DiscoveryHttpError(
                status = response.status.value,
                endpoint = "$endpoint (POST)",
                body = response.safeReadBody(),
            )
        }
        return response.body<InviteOwnerListResponse>()
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
     * Upload a freshly-composed [InviteCapability] blob and
     * receive the server-assigned content-addressed `blobId`
     * (raw 32-byte SHA-256). Drives one PoW solve + `POST
     * /discovery/invite`. Idempotent — same bytes yield the
     * same id, server upserts.
     *
     * Feed `capabilityBytes` straight from
     * [uniffi.lakko_ffi_uniffi.composeInviteCapability]. The
     * server runs `decode_and_verify` server-side, so a junk
     * upload surfaces as
     * `DiscoveryHttpError(400, "/discovery/invite", …)`.
     *
     * Returns the 32-byte `blobId`, ready to substitute into a
     * `https://<host>/i/<slug>` pointer URL via
     * [base64UrlEncodeNoPad].
     */
    public suspend fun uploadInvite(capabilityBytes: ByteArray): ByteArray {
        if (capabilityBytes.isEmpty()) {
            throw DiscoveryClientArgError("capabilityBytes must be non-empty")
        }
        if (capabilityBytes.size > MAX_CAPABILITY_BYTES) {
            // Mirror the server-side ceiling so a misbehaving
            // caller fails fast without a round-trip.
            throw DiscoveryClientArgError(
                "capabilityBytes exceeds the $MAX_CAPABILITY_BYTES-byte ceiling " +
                    "(got ${capabilityBytes.size} bytes)",
            )
        }

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val resp = inviteUpload(
            InviteUploadRequest(pow = pow, capability = base64Encode(capabilityBytes)),
        )
        // Server returns base64url-no-pad; decode back to raw
        // 32 bytes via the same helper used for accountId
        // slugs.
        val blobId = base64UrlDecodeNoPad(resp.blob_id)
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryHttpError(
                status = 200,
                endpoint = "/discovery/invite",
                body = "server returned blob_id of ${blobId.size} bytes, expected $BLOB_ID_LEN",
            )
        }
        return blobId
    }

    /**
     * Fetch a previously-uploaded `InviteCapability` by its
     * content-addressed [blobId]. Returns `null` on 404 (server
     * folds expired blobs into the same status), the raw
     * capability bytes on 200 (the caller drives
     * [uniffi.lakko_ffi_uniffi.InviteCapability.decodeAndVerify]
     * to re-verify locally so a malicious server cannot serve
     * substituted bytes).
     */
    public suspend fun fetchInvite(blobId: ByteArray): ByteArray? {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        val raw = inviteFetch(blobId) ?: return null
        return base64Decode(raw.capability)
    }

    /**
     * Atomically bump the consume counter for the invite at
     * [blobId] with per-joiner dedup. Returns a discriminated
     * [ConsumeInviteResult] so the joiner UI can branch on the
     * four observable outcomes without exception-walking the
     * underlying [DiscoveryHttpError] hierarchy.
     *
     * Drives one PoW solve + one
     * [uniffi.lakko_ffi_uniffi.computeJoinerInviteTag] call
     * locally before posting; the joiner's `accountId` never
     * crosses the wire in plaintext.
     *
     * @param capabilityBytes the same wire-format bytes the
     *        joiner already verified locally via
     *        `InviteCapability.decodeAndVerify` — mandatory
     *        for the dedup-tag's per-blob salt derivation.
     * @param joinerAccountId the joiner's 16-byte UUIDv7
     *        account_id. Hashed locally into the tag; not sent.
     */
    public suspend fun consumeInvite(
        blobId: ByteArray,
        capabilityBytes: ByteArray,
        joinerAccountId: ByteArray,
    ): ConsumeInviteResult {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        if (joinerAccountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("joinerAccountId must be $ACCOUNT_ID_LEN bytes")
        }

        val joinerTag = computeJoinerInviteTag(capabilityBytes, joinerAccountId)
        val slug = base64UrlEncodeNoPad(blobId)

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val endpoint = "/discovery/invite/$slug/consume"
        val url = "$baseUrl$endpoint"
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.post(url) {
                if (authHeader != null) {
                    header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                }
                contentType(ContentType.Application.Json)
                setBody(
                    InviteConsumeRequest(
                        pow = pow,
                        joiner_tag = base64Encode(joinerTag),
                    ),
                )
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError(endpoint, cause)
        }
        return when (response.status) {
            HttpStatusCode.NotFound -> ConsumeInviteResult.NotFound
            HttpStatusCode.Gone -> ConsumeInviteResult.Exhausted
            else -> {
                if (!response.status.isSuccess()) {
                    throw DiscoveryHttpError(
                        status = response.status.value,
                        endpoint = endpoint,
                        body = response.safeReadBody(),
                    )
                }
                val parsed: InviteConsumeResponse = response.body()
                if (parsed.already_consumed) {
                    ConsumeInviteResult.AlreadyConsumed(
                        usedCount = parsed.used_count,
                        maxUses = parsed.max_uses,
                    )
                } else {
                    ConsumeInviteResult.Consumed(
                        usedCount = parsed.used_count,
                        maxUses = parsed.max_uses,
                    )
                }
            }
        }
    }

    /**
     * Owner-signed revocation of an invite blob.
     *
     * Driven by the same Ed25519 key that signed the
     * capability:
     * [uniffi.lakko_ffi_uniffi.signInviteRevocation] builds the
     * canonical bytes + signs. We POST `signed_at_ms +
     * signature` as the DELETE body; the server recovers the
     * inviter's pubkey from the stored capability bytes and
     * verifies the signature against the same canonical
     * layout.
     *
     * Replay window: the server enforces ±10 min against its
     * own clock. The client uses [System.currentTimeMillis]
     * directly — a machine with a clock skewed more than ~10
     * min will see 400-bad-window failures, mirroring the
     * consume path.
     *
     * Returns a discriminated [RevokeInviteResult]: `Revoked`
     * on 2xx, `NotFoundOrNotOwner` on 404 (server folds "no
     * row" / "bad signature" / "stored bytes failed re-decode"
     * into one status to deny status-code probing). Other
     * non-2xx still throw [DiscoveryHttpError].
     */
    public suspend fun revokeInvite(
        blobId: ByteArray,
        signingSeed: ByteArray,
    ): RevokeInviteResult {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        if (signingSeed.size != SIGNING_SEED_LEN) {
            throw DiscoveryClientArgError("signingSeed must be $SIGNING_SEED_LEN bytes")
        }

        // Pin signedAtMs to one value across the await
        // boundary — reading System.currentTimeMillis twice
        // could yield distinct millis. The server's ±10-min
        // window absorbs the difference but pinning removes
        // doubt.
        val signedAtMs = System.currentTimeMillis()
        val signature = signInviteRevocation(signingSeed, blobId, signedAtMs.toULong())
        val slug = base64UrlEncodeNoPad(blobId)
        return try {
            inviteRevoke(
                slug,
                InviteRevokeRequest(
                    signed_at_ms = signedAtMs,
                    signature = base64Encode(signature),
                ),
            )
            RevokeInviteResult.Revoked
        } catch (e: DiscoveryHttpError) {
            if (e.status == 404) RevokeInviteResult.NotFoundOrNotOwner else throw e
        }
    }

    /**
     * Upload a freshly-composed `CallInviteCapability` blob and
     * receive the server-assigned content-addressed `blobId`
     * (raw 32-byte SHA-256). Drives one PoW solve + `POST
     * /discovery/call-invite`. Idempotent.
     *
     * Sibling of [uploadInvite]; the client-side ceiling is
     * [MAX_CALL_CAPABILITY_BYTES] (8 KiB, 4× contact-invite)
     * because the call-invite capability embeds a one-time
     * MLS KeyPackage.
     *
     * Returns the 32-byte `blobId`, ready to substitute into a
     * `https://<host>/c/<slug>` pointer URL via
     * [base64UrlEncodeNoPad].
     */
    public suspend fun uploadCallInvite(capabilityBytes: ByteArray): ByteArray {
        if (capabilityBytes.isEmpty()) {
            throw DiscoveryClientArgError("capabilityBytes must be non-empty")
        }
        if (capabilityBytes.size > MAX_CALL_CAPABILITY_BYTES) {
            throw DiscoveryClientArgError(
                "capabilityBytes exceeds the $MAX_CALL_CAPABILITY_BYTES-byte ceiling " +
                    "(got ${capabilityBytes.size} bytes)",
            )
        }

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val resp = callInviteUpload(
            InviteUploadRequest(pow = pow, capability = base64Encode(capabilityBytes)),
        )
        val blobId = base64UrlDecodeNoPad(resp.blob_id)
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryHttpError(
                status = 200,
                endpoint = "/discovery/call-invite",
                body = "server returned blob_id of ${blobId.size} bytes, expected $BLOB_ID_LEN",
            )
        }
        return blobId
    }

    /**
     * Fetch a previously-uploaded `CallInviteCapability` by its
     * content-addressed [blobId]. Returns `null` on 404 (server
     * folds expired blobs into the same status), raw capability
     * bytes on 200 (the caller drives
     * [uniffi.lakko_ffi_uniffi.CallInviteCapability.decodeAndVerify]
     * to re-verify locally so a malicious server cannot serve
     * substituted bytes).
     */
    public suspend fun fetchCallInvite(blobId: ByteArray): ByteArray? {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        val raw = callInviteFetch(blobId) ?: return null
        return base64Decode(raw.capability)
    }

    /**
     * Atomically bump the consume counter for the call-invite
     * at [blobId] with per-joiner dedup. Returns a discriminated
     * [ConsumeInviteResult] so the joiner UI can branch on the
     * four observable outcomes.
     *
     * Reuses the same `computeJoinerInviteTag(...)` helper as
     * the contact-invite flow — salt is per-blob, so
     * contact-invite and call-invite tags for the same joiner
     * don't collide on the server.
     */
    public suspend fun consumeCallInvite(
        blobId: ByteArray,
        capabilityBytes: ByteArray,
        joinerAccountId: ByteArray,
    ): ConsumeInviteResult {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        if (joinerAccountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("joinerAccountId must be $ACCOUNT_ID_LEN bytes")
        }

        val joinerTag = computeJoinerInviteTag(capabilityBytes, joinerAccountId)
        val slug = base64UrlEncodeNoPad(blobId)

        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        val endpoint = "/discovery/call-invite/$slug/consume"
        val url = "$baseUrl$endpoint"
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.post(url) {
                if (authHeader != null) {
                    header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                }
                contentType(ContentType.Application.Json)
                setBody(
                    InviteConsumeRequest(
                        pow = pow,
                        joiner_tag = base64Encode(joinerTag),
                    ),
                )
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError(endpoint, cause)
        }
        return when (response.status) {
            HttpStatusCode.NotFound -> ConsumeInviteResult.NotFound
            HttpStatusCode.Gone -> ConsumeInviteResult.Exhausted
            else -> {
                if (!response.status.isSuccess()) {
                    throw DiscoveryHttpError(
                        status = response.status.value,
                        endpoint = endpoint,
                        body = response.safeReadBody(),
                    )
                }
                val parsed: InviteConsumeResponse = response.body()
                if (parsed.already_consumed) {
                    ConsumeInviteResult.AlreadyConsumed(
                        usedCount = parsed.used_count,
                        maxUses = parsed.max_uses,
                    )
                } else {
                    ConsumeInviteResult.Consumed(
                        usedCount = parsed.used_count,
                        maxUses = parsed.max_uses,
                    )
                }
            }
        }
    }

    /**
     * Owner-signed revocation of a [CallInviteCapability] blob
     * (Phase 3c3-revoke).
     *
     * Sibling of [revokeInvite] for the call-invite ecosystem.
     * Driven by the same Ed25519 key that signed the capability:
     * [uniffi.lakko_ffi_uniffi.signCallInviteRevocation] builds
     * the canonical bytes
     * (`CALL_INVITE_REVOKE_DOMAIN_V1 || blob_id ||
     * signed_at_ms_be`) under a distinct domain separator so a
     * captured contact-invite revoke cannot be replayed here
     * (and vice versa). The wire body shape is the shared
     * [InviteRevokeRequest] — only the URL path differs.
     *
     * Returns a discriminated [RevokeInviteResult]: `Revoked`
     * on 2xx, `NotFoundOrNotOwner` on 404 (server folds "no
     * row" / "bad signature" / "stored bytes failed re-decode"
     * into one status). Other non-2xx still throw
     * [DiscoveryHttpError].
     */
    public suspend fun revokeCallInvite(
        blobId: ByteArray,
        signingSeed: ByteArray,
    ): RevokeInviteResult {
        if (blobId.size != BLOB_ID_LEN) {
            throw DiscoveryClientArgError("blobId must be $BLOB_ID_LEN bytes")
        }
        if (signingSeed.size != SIGNING_SEED_LEN) {
            throw DiscoveryClientArgError("signingSeed must be $SIGNING_SEED_LEN bytes")
        }

        val signedAtMs = System.currentTimeMillis()
        val signature = signCallInviteRevocation(signingSeed, blobId, signedAtMs.toULong())
        val slug = base64UrlEncodeNoPad(blobId)
        return try {
            callInviteRevoke(
                slug,
                InviteRevokeRequest(
                    signed_at_ms = signedAtMs,
                    signature = base64Encode(signature),
                ),
            )
            RevokeInviteResult.Revoked
        } catch (e: DiscoveryHttpError) {
            if (e.status == 404) RevokeInviteResult.NotFoundOrNotOwner else throw e
        }
    }

    /**
     * Owner-signed list of every live invite this account owns
     * across all devices (Phase 1.18d2b-list).
     *
     * Driven by the same Ed25519 identity key the owner used to
     * sign every [InviteCapability] they own. Distinct domain
     * separator from [signInviteRevocation] ensures a captured
     * revoke signature cannot be replayed against this endpoint.
     * Replay window: server enforces ±10 min against its own
     * clock — same constant as revoke.
     *
     * Returns a discriminated [ListInvitesResult]:
     *   * `List(rows)` on 200 (empty list when the owner has no
     *     live invites — distinct from a malformed-request
     *     failure).
     *   * `NotFoundOrNotOwner` on 404 (server folds "no
     *     identity_bundle for `accountId`" / "signature mismatch"
     *     / "stored pubkey malformed" into one status to deny
     *     status-code probing).
     *
     * Other non-2xx still throw [DiscoveryHttpError].
     */
    public suspend fun listInvitesByOwner(
        accountId: ByteArray,
        signingSeed: ByteArray,
    ): ListInvitesResult {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (signingSeed.size != SIGNING_SEED_LEN) {
            throw DiscoveryClientArgError("signingSeed must be $SIGNING_SEED_LEN bytes")
        }

        val signedAtMs = System.currentTimeMillis()
        val signature = signOwnerListQuery(signingSeed, accountId, signedAtMs.toULong())
        val raw = inviteOwnerList(
            InviteOwnerListRequest(
                owner_account_id = base64Encode(accountId),
                signed_at_ms = signedAtMs,
                signature = base64Encode(signature),
            ),
        ) ?: return ListInvitesResult.NotFoundOrNotOwner

        val rows = raw.invites.map { entry ->
            ActiveInvite(
                blobIdSlug = entry.blob_id_slug,
                expiryMs = entry.expiry_ms,
                maxUses = entry.max_uses,
                usedCount = entry.used_count,
                label = entry.label,
                bootstrapReferral = entry.bootstrap_referral,
                createdAt = entry.created_at,
            )
        }
        return ListInvitesResult.List(rows)
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
     *
     * G-4 (ADR-0016): [supersessionProofB64], when non-null, is the
     * old-key-signed proof bridging an AccountIdentity rotation
     * (recover). It rides straight onto
     * [IdentityUploadRequest.supersession_proof] so the new keys +
     * proof ship in the same request (ADR-0016 rule #1, atomic
     * emit). The server stores it opaquely and clears it on a
     * non-rotation re-upload. Non-rotation callers (signup / login /
     * passkey) pass `null` — a normal upload clears any stale
     * server-side proof.
     */
    public suspend fun uploadIdentityBundle(
        accountId: ByteArray,
        signaturePubkey: ByteArray,
        x25519Pubkey: ByteArray,
        supersessionProofB64: String? = null,
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
                supersession_proof = supersessionProofB64,
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
        // Phase 1.5b: capture the current bearer token outside
        // the (potentially Dispatchers.IO-running) Ktor block so
        // the inline lambda doesn't try to capture `this` —
        // `@Volatile` reads are atomic but the field-access
        // shape still confuses inlining.
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.post(url) {
                if (authHeader != null) {
                    header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                }
                configure()
            }
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
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.post(url) {
                if (authHeader != null) {
                    header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                }
                configure()
            }
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
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.get(url) {
                if (authHeader != null) {
                    header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                }
            }
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

        /** Mirror of `services/discovery`'s
         *  `MAX_CAPABILITY_BYTES`. The 1.18a wire format is
         *  small (a few hundred bytes); the 2048 ceiling has
         *  generous headroom. */
        private const val MAX_CAPABILITY_BYTES: Int = 2048

        /** Mirror of `services/discovery`'s
         *  `MAX_CALL_CAPABILITY_BYTES`. The call-invite
         *  ledger uses an 8 KiB ceiling (vs. contact-invite's
         *  2 KiB) because `CallInviteCapability` embeds a
         *  one-time MLS KeyPackage (~4 KiB upper bound per
         *  `MAX_KEY_PACKAGE_LEN`). */
        private const val MAX_CALL_CAPABILITY_BYTES: Int = 8192

        /** SHA-256 → 32-byte content-addressed invite id. */
        private const val BLOB_ID_LEN: Int = 32

        /** Ed25519 signing seed length, mirrors
         *  `me.lakko.app.auth` constants. */
        private const val SIGNING_SEED_LEN: Int = 32
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

/**
 * Result of [DiscoveryClient.consumeInvite]. Discriminated so
 * the joiner UI can branch cleanly on the four observable
 * outcomes without exception-walking the underlying
 * [DiscoveryHttpError] hierarchy:
 *
 *   * [Consumed] — server accepted the consume + bumped the
 *     counter + recorded this joiner in the dedup side table.
 *     Caller proceeds with `sendContactRequestToBundle(...)`.
 *   * [AlreadyConsumed] — server's `(blob_id, joiner_tag)` PK
 *     already had a row, so the counter was NOT bumped. UI
 *     surfaces "you've already accepted this invite" +
 *     Decline-only; skipping the ContactRequest send avoids
 *     landing another envelope in the inviter's stranger
 *     inbox.
 *   * [Exhausted] — 410 Gone, the `max_uses` ceiling was hit
 *     by other joiners before this consume could land.
 *   * [NotFound] — 404, never-issued OR expired (server folds
 *     both into the same status to deny status-code probing).
 *     UI surfaces "invite expired or revoked".
 */
public sealed interface ConsumeInviteResult {
    public data class Consumed(
        val usedCount: Int,
        val maxUses: Long,
    ) : ConsumeInviteResult

    public data class AlreadyConsumed(
        val usedCount: Int,
        val maxUses: Long,
    ) : ConsumeInviteResult

    public data object Exhausted : ConsumeInviteResult

    public data object NotFound : ConsumeInviteResult
}

/**
 * Result of [DiscoveryClient.revokeInvite]. Discriminated so
 * the owner UI can branch cleanly on the two observable
 * outcomes:
 *
 *   * [Revoked] — server deleted the row (2xx).
 *   * [NotFoundOrNotOwner] — 404. Server folds "no row" /
 *     "signature did not verify" / "stored bytes failed
 *     re-decode" into the same status to deny status-code
 *     probing of the blob_id space; the UI surfaces a generic
 *     "this invite no longer exists or you don't own it"
 *     message.
 *
 * Body-shape failures (bad base64, signed_at_ms outside the
 * replay window, …) still throw `DiscoveryHttpError(400, …)`
 * — those indicate a buggy client, not a probing attacker.
 */
public sealed interface RevokeInviteResult {
    public data object Revoked : RevokeInviteResult
    public data object NotFoundOrNotOwner : RevokeInviteResult
}

/** One row in [DiscoveryClient.listInvitesByOwner]'s response —
 *  normalised TS-parity view of the wire shape (Phase
 *  1.18d2b-list). `blobIdSlug` is the URL-safe base64 the
 *  owner uses to reconstruct the share URL or feed back into
 *  [DiscoveryClient.revokeInvite]. */
public data class ActiveInvite(
    /** URL-safe base64 (no padding) of the 32-byte blob_id —
     *  same `/i/<slug>` segment that appears in the share URL. */
    val blobIdSlug: String,
    /** UNIX millis the capability expires at. `0` means never
     *  expires (the `NEVER_EXPIRES` sentinel). */
    val expiryMs: Long,
    /** Wire-format `max_uses` sentinel: `0` = SINGLE_USE,
     *  `0xFFFFFFFF` = unlimited, any other N = N consumes
     *  allowed. */
    val maxUses: Long,
    /** Server-counted consumes. */
    val usedCount: Long,
    /** Owner-supplied free-text label; empty string when not
     *  set. */
    val label: String,
    /** `true` if the capability flags the link as a referral
     *  signup-deep-link (Phase 1.9b4). */
    val bootstrapReferral: Boolean,
    /** RFC 3339 timestamp the server recorded on first upload. */
    val createdAt: String,
)

/** Result of [DiscoveryClient.listInvitesByOwner]. */
public sealed interface ListInvitesResult {
    /** Server returned the live-invite list (possibly empty). */
    public data class List(val invites: kotlin.collections.List<ActiveInvite>) :
        ListInvitesResult
    /** 404. Server folds "no identity_bundle for `accountId`" /
     *  "signature mismatch" / "stored pubkey malformed" into one
     *  status to deny status-code probing. */
    public data object NotFoundOrNotOwner : ListInvitesResult
}

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
