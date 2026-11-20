// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.serialization.json.Json
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.sendWithToken
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import uniffi.lakko_ffi_uniffi.OprfBlinder
import uniffi.lakko_ffi_uniffi.computeJoinerInviteTag
import uniffi.lakko_ffi_uniffi.signCallInviteRevocation
import uniffi.lakko_ffi_uniffi.signGroupInviteRevocation
import uniffi.lakko_ffi_uniffi.signInviteRevocation

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
 *      flows: OPRF blind → POST `/oprf` → finalize → POST
 *      `/register` (or `/lookup`).
 *
 * ## Anonymous tokens, no proof of work (ADR-0054 S3)
 *
 * The OPRF evaluation spends a `discovery-lookup` token, a channel-name
 * registration a `discovery-register` token, and each of the three invite
 * consumes an `invite-consume` token, drawn from [anonTokens]. Those routes,
 * and the two lookups that follow an evaluation, go out on a SEPARATE HTTP
 * client ([AnonymousHttp]) that never carries `Authorization`: a session on
 * them would name the account next to what it looked up. The bearer routes
 * (register, identity, KeyPackage, invite uploads) carry only the session.
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
    /** Engine for the anonymous routes (the token routes and the two
     *  lookups). Production: [AnonymousHttp.engine], its own connection pool.
     *  Tests: `null` reuses [engine] (a `MockEngine`). */
    anonEngine: HttpClientEngine? = null,
    /** Where the token routes draw their anonymous token (ADR-0054). */
    anonTokens: AnonTokenSource = AnonTokens.shared,
    /** The short-link widths (ADR-0028 amendment of 2026-09-27): the 6-byte
     *  ledger id every invite route takes. Defaults to the core's values;
     *  a host JVM test (no native library) passes them in. */
    private val inviteLinkLengths: () -> InviteLinkLengths = { InviteLinkLengths.core },
) : AutoCloseable {

    /** Width in bytes of a ledger id (6), read from the core. */
    private val blobIdLen: Int get() = inviteLinkLengths().blobIdLen

    /** Decode the `blob_id` an upload answered with; a width other than the
     *  8-character / 6-byte id is the server misbehaving, never a link. */
    private fun uploadedBlobId(blobIdText: String, endpoint: String): ByteArray =
        decodeInviteBlobId(blobIdText, inviteLinkLengths())
            ?: throw DiscoveryHttpError(
                status = 200,
                endpoint = endpoint,
                body = "server returned a blob_id of ${blobIdText.length} characters, expected " +
                    "${inviteLinkLengths().blobIdUrlLen}",
            )

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    // ADR-0026 Phase 4 — the production engine wraps OkHttp in the
    // anti-censorship engine, which routes through the obfuscated tunnel when
    // the ambient config is armed and delegates to OkHttp verbatim otherwise
    // (so the default path is unchanged). A test [engine] bypasses it.
    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) {
            install(ContentNegotiation) { json(json) }
        }
    } else {
        HttpClient(lakkoHttpEngine()) {
            install(ContentNegotiation) { json(json) }
        }
    }

    /** The client for every anonymous route. It never attaches
     *  `Authorization`, and in production it has its own connection pool, so
     *  a token route cannot ride a connection an authenticated call used. */
    private val anonClient: HttpClient =
        HttpClient(anonEngine ?: engine ?: AnonymousHttp.engine) {
            install(ContentNegotiation) { json(json) }
        }

    /**
     * Phase 1.5b: bearer token threaded into the
     * `Authorization: Bearer <token>` header on every protected
     * `lakko-svc-discovery` write. `null` means "anonymous";
     * the anonymous routes never carry it (they go out on the
     * anonymous client), and the GETs work without it.
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

    @Volatile
    private var anonTokens: AnonTokenSource = anonTokens

    /**
     * Draw this client's anonymous tokens with [session] rather than the live
     * one: signup claims its username before the new session is installed.
     * The tokens still go out on the anonymous client; only who they are
     * ISSUED to changes.
     */
    public fun useTokenSession(session: me.lakko.app.anon.AnonSession) {
        anonTokens = me.lakko.app.anon.DetachedAnonTokenSource(anonTokens, session)
    }

    override fun close() {
        client.close()
        anonClient.close()
    }

    // -----------------------------------------------------------------
    // Low-level: 1:1 HTTP endpoints
    // -----------------------------------------------------------------

    /** `POST /discovery/oprf`, spending one `discovery-lookup` token. */
    public suspend fun oprfEvaluate(req: OprfRequest): OprfResponse =
        tokenPost("/discovery/oprf", AnonTokenContext.DISCOVERY_LOOKUP) {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/oprf/batch`: the own name's terms in one request, one
     *  `discovery-register` token, no session. */
    public suspend fun oprfEvaluateBatch(req: OprfBatchRequest): OprfBatchResponse =
        tokenPost("/discovery/oprf/batch", AnonTokenContext.DISCOVERY_REGISTER) {
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
        doPostNullableOn404("/discovery/lookup", anonymous = true) {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/channel/register` — bind an OPRF-hashed
     *  `@channelname` to a `channel_id` (G4-5, ADR-0010). Anonymous: spends
     *  one `discovery-register` token (ADR-0054). */
    public suspend fun channelRegister(req: ChannelRegisterRequest): ChannelRegisterResponse =
        tokenPost("/discovery/channel/register", AnonTokenContext.DISCOVERY_REGISTER) {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `POST /discovery/channel/lookup`. Returns the typed response
     * on 200, `null` on 404 (unknown `@channelname`), throws on
     * any other non-2xx. Mirror of [lookupRaw] for the channel
     * namespace (G4-5, ADR-0010).
     */
    public suspend fun channelLookupRaw(req: ChannelLookupRequest): ChannelLookupResponse? =
        doPostNullableOn404("/discovery/channel/lookup", anonymous = true) {
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
        val bundle: IdentityFetchResponse? = getNullableOn404(endpoint)
        // ADR-0098 section 2 — every complete answer (200 or 404) is
        // recorded for the person this device holds under that account id;
        // a thrown error is not an answer and records nothing.
        me.lakko.app.profile.DirectoryAnswerFeed.report(
            accountId.copyOf(),
            bundle?.signature_pubkey,
            bundle?.conversation_link,
        )
        return bundle
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
     *  `InviteCapability` blob, get the server-assigned
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val slug = base64UrlEncodeNoPad(blobId)
        val endpoint = "/discovery/invite/$slug"
        return getNullableOn404(endpoint)
    }

    /**
     * `POST /discovery/invite/{blob_id_b64url}/consume` —
     * atomic `max_uses` enforcement + per-joiner dedup
     * (Phase 1.18d2a + 1.18d3b). The high-level [consumeInvite]
     * wrapper draws the `invite-consume` token + the local
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
        tokenPost("/discovery/invite/$blobIdSlug/consume", AnonTokenContext.INVITE_CONSUME) {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /** `POST /discovery/call-invite` — upload a verified
     *  `CallInviteCapability` blob, get the server-assigned
     *  `blob_id` back (Phase 3c3-server). Sibling of
     *  [inviteUpload]; wire shape is shared. The high-level
     *  [uploadCallInvite] wrapper is the session-gated write. */
    public suspend fun callInviteUpload(req: CallInviteUploadRequest): InviteUploadResponse =
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val slug = base64UrlEncodeNoPad(blobId)
        val endpoint = "/discovery/call-invite/$slug"
        return getNullableOn404(endpoint)
    }

    /**
     * `POST /discovery/call-invite/{blob_id_b64url}/consume` —
     * atomic `max_uses` enforcement + per-joiner dedup against
     * the call-invite ledger (Phase 3c3-multi-use). The high-
     * level [consumeCallInvite] wrapper draws the token +
     * the local `computeJoinerInviteTag` call + folds the 200 /
     * 404 / 410 status trifecta into a discriminated
     * [ConsumeInviteResult].
     */
    public suspend fun callInviteConsume(
        blobIdSlug: String,
        req: InviteConsumeRequest,
    ): InviteConsumeResponse =
        tokenPost("/discovery/call-invite/$blobIdSlug/consume", AnonTokenContext.INVITE_CONSUME) {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    // ------------------------------------------------------------------------
    // ADR-0028 (G-3) — group join-ticket blob ledger
    // ------------------------------------------------------------------------

    /** `POST /discovery/group-invite` — upload a verified
     *  `GroupInviteCapability` blob, get the server-assigned `blob_id` back
     *  (ADR-0028, G-3). Distinct from [callInviteUpload] because the
     *  group-invite request carries a `max_uses` the capability does NOT embed.
     *  The high-level [uploadGroupInvite] wrapper is the session-gated write. */
    public suspend fun groupInviteUpload(req: GroupInviteUploadRequest): InviteUploadResponse =
        doPost("/discovery/group-invite") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }

    /**
     * `GET /discovery/group-invite/{blob_id_b64url}`. Returns the capability
     * wire shape on 200, `null` on 404 (the server folds expired blobs into the
     * same status), throws on any other non-2xx.
     */
    public suspend fun groupInviteFetch(blobId: ByteArray): InviteFetchResponse? {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val slug = base64UrlEncodeNoPad(blobId)
        val endpoint = "/discovery/group-invite/$slug"
        return getNullableOn404(endpoint)
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

    /** A3 (GDPR erasure) — `DELETE /discovery/account`. Purges this account's
     *  discovery footprint (username binding, identity bundle, KeyPackage,
     *  owned invites). Bearer-auth; the server keys the purge on the verified
     *  session token's account_id. Idempotent (204). */
    public suspend fun deleteAccount() {
        val endpoint = "/discovery/account"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.delete(url) {
                sessionToken?.let {
                    header(io.ktor.http.HttpHeaders.Authorization, "Bearer $it")
                }
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (DELETE)", cause)
        }
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

    // -----------------------------------------------------------------
    // High-level: full register / lookup / upload flows
    // -----------------------------------------------------------------

    /**
     * Bind `@username` to [accountId] server-side.
     *
     * Flow: OPRF blind → `POST /discovery/oprf` (one `discovery-lookup`
     * token) → finalize → `POST /discovery/register` (the session).
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

            val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))

            // ADR-0060 §5: discovery receives the lookup key derived
            // from the OPRF output, never the output itself.
            val lookupKeyB64 = base64Encode(blinder.finalize(oprfResp.evaluated).lookupKey)

            val resp = register(
                RegisterRequest(
                    account_id = base64Encode(accountId),
                    oprf_hash = lookupKeyB64,
                ),
            )
            if (!resp.ok) {
                throw DiscoveryHttpError(
                    status = 200,
                    endpoint = "/discovery/register",
                    body = "server reported ok=false",
                )
            }
            RegisterResult(
                normalisedUsername = normalisedUsername,
                // ADR-0060 section 5 - the certificate the register call
                // returned is kept (a malformed one is as good as none).
                usernameCertificate = resp.username_certificate_b64?.let {
                    try {
                        base64Decode(it)
                    } catch (_: IllegalArgumentException) {
                        null
                    }
                },
            )
        }
    }

    /**
     * ADR-0076 - replace the account's current username with [username]:
     * OPRF, then `POST /discovery/username/replace`. The old name is retired
     * (nobody finds the account by it; the account holds at most one retired
     * name, and the server frees older ones on each rename; no minimum interval), and
     * the account leaves Everyone on the server until the client publishes
     * its public record again. Returns the claimed name and the NEW
     * username certificate.
     *
     * @throws DiscoveryHttpError `409 no-username`, `409 username-taken`.
     */
    public suspend fun replaceUsername(accountId: ByteArray, username: String): RegisterResult {
        require(accountId.size == 16) { "accountId must be 16 bytes (got ${accountId.size})" }
        return OprfBlinder.blind(username).use { blinder ->
            val normalisedUsername = blinder.normalizedUsername()
            val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))
            val lookupKeyB64 = base64Encode(blinder.finalize(oprfResp.evaluated).lookupKey)
            val resp: RegisterResponse = doPost("/discovery/username/replace") {
                contentType(ContentType.Application.Json)
                setBody(RegisterRequest(account_id = base64Encode(accountId), oprf_hash = lookupKeyB64))
            }
            if (!resp.ok) {
                throw DiscoveryHttpError(
                    status = 200,
                    endpoint = "/discovery/username/replace",
                    body = "server reported ok=false",
                )
            }
            RegisterResult(
                normalisedUsername = normalisedUsername,
                usernameCertificate = resp.username_certificate_b64?.let {
                    try {
                        base64Decode(it)
                    } catch (_: IllegalArgumentException) {
                        null
                    }
                },
            )
        }
    }

    /**
     * Resolve `@username` to a raw 16-byte account_id, or `null`
     * when the username is not registered.
     *
     * Same flow as [registerUsername] modulo the second endpoint:
     * OPRF blind → `POST /discovery/oprf` (one token) → finalize →
     * `POST /discovery/lookup` (anonymous, no token).
     */
    public suspend fun lookup(username: String): LookupResult {
        return OprfBlinder.blind(username).use { blinder ->
            val normalisedUsername = blinder.normalizedUsername()

            val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))

            val lookupKeyB64 = base64Encode(blinder.finalize(oprfResp.evaluated).lookupKey)

            val resp = lookupRaw(LookupRequest(oprf_hash = lookupKeyB64))
            val accountB64 = resp?.account_id?.takeIf { resp.kind != "handle" }
            if (resp == null || accountB64 == null) {
                LookupResult(normalisedUsername = normalisedUsername, account = null)
            } else {
                LookupResult(
                    normalisedUsername = normalisedUsername,
                    account = base64Decode(accountB64),
                )
            }
        }
    }

    // -----------------------------------------------------------------
    // ADR-0060 §5 — username certificate, Everyone, name search
    // -----------------------------------------------------------------

    /**
     * `GET /discovery/username-certificate` (session-gated): this account's
     * certificate, signed now, or `null` when it has no username (404).
     */
    public suspend fun fetchUsernameCertificate(): ByteArray? {
        val resp = getNullableOn404<UsernameCertificateResponse>("/discovery/username-certificate")
            ?: return null
        return base64Decode(resp.username_certificate_b64)
    }

    /** `GET /.well-known/lakko-username-cert-key`: the certificate verifying
     *  key (32 bytes), for a build without a pinned one. */
    public suspend fun fetchUsernameCertKey(): ByteArray? {
        val resp = getNullableOn404<UsernameCertKeyResponse>("/.well-known/lakko-username-cert-key")
            ?: return null
        if (resp.alg != "Ed25519") return null
        return try {
            me.lakko.app.auth.base64UrlDecodeNoPad(resp.key_b64url).takeIf { it.size == PUBKEY_LEN }
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /**
     * `PUT /discovery/visibility` (session-gated): enter Everyone with the
     * discovery lookup key of every name search term ([nameTermLookupKeys],
     * 64 bytes each, `OprfResult.lookupKey` — never the OPRF output) and the
     * [publicRecord] composed over the outputs (required by the server in
     * Everyone), or leave it ([public] `false`, no terms, no record).
     * Idempotent.
     */
    public suspend fun putVisibility(
        public: Boolean,
        nameTermLookupKeys: List<ByteArray>,
        publicRecord: ByteArray? = null,
    ) {
        val endpoint = "/discovery/visibility"
        val authHeader = sessionToken?.let { "Bearer $it" }
        val response: HttpResponse = try {
            client.put("$baseUrl$endpoint") {
                if (authHeader != null) header(io.ktor.http.HttpHeaders.Authorization, authHeader)
                contentType(ContentType.Application.Json)
                setBody(visibilityRequest(public, nameTermLookupKeys, publicRecord))
            }
        } catch (cause: CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError(endpoint, cause)
        }
        if (!response.status.isSuccess()) {
            throw DiscoveryHttpError(response.status.value, endpoint, response.safeReadBody())
        }
    }

    /**
     * OPRF one name search term (one `discovery-lookup` token). ADR-0060 §5:
     * [uniffi.lakko_ffi_uniffi.OprfResult.lookupKey] is what a name lookup and
     * the Everyone registration send; [uniffi.lakko_ffi_uniffi.OprfResult.output]
     * stays on the device (public-record and introduction key material).
     */
    public suspend fun oprfNameTerm(term: String): uniffi.lakko_ffi_uniffi.OprfResult =
        uniffi.lakko_ffi_uniffi.OprfBlinder.blindNameTerm(term).use { blinder ->
            val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))
            blinder.finalize(oprfResp.evaluated)
        }

    /**
     * OPRF a username (one `discovery-lookup` token). The certificate binds
     * [uniffi.lakko_ffi_uniffi.OprfResult.lookupKey]; the output never leaves
     * the device.
     */
    public suspend fun oprfUsername(username: String): uniffi.lakko_ffi_uniffi.OprfResult =
        // who-can-find-me.md 6.1 — the same text is not asked twice within 60
        // seconds: a taken check and the claim at Create share one answer.
        recentOprf.getOrAsk(username) {
            uniffi.lakko_ffi_uniffi.OprfBlinder.blind(username).use { blinder ->
                val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))
                blinder.finalize(oprfResp.evaluated)
            }
        }

    /**
     * Publishing an own name (a group's or channel's @name and name terms, the
     * account's public record, its introduction terms): every term blinded,
     * sent in batches of at most [OPRF_BATCH_MAX] (66) through
     * `POST /discovery/oprf/batch` (one `discovery-register` token per batch),
     * each finalized with its own blinder. Results in the order of [terms].
     * Searches for others stay on [oprfUsername] / [oprfNameTerm].
     */
    public suspend fun oprfOwnTerms(terms: List<OwnTerm>): List<uniffi.lakko_ffi_uniffi.OprfResult> {
        val out = ArrayList<uniffi.lakko_ffi_uniffi.OprfResult>(terms.size)
        for (chunk in terms.chunked(OPRF_BATCH_MAX)) {
            val blinders = chunk.map { t ->
                when (t) {
                    is OwnTerm.Username -> uniffi.lakko_ffi_uniffi.OprfBlinder.blind(t.value)
                    is OwnTerm.NameTerm -> uniffi.lakko_ffi_uniffi.OprfBlinder.blindNameTerm(t.value)
                }
            }
            try {
                val resp = oprfEvaluateBatch(OprfBatchRequest(blinded = blinders.map { it.blindedB64() }))
                if (resp.evaluated.size != blinders.size) {
                    throw DiscoveryHttpError(200, "/discovery/oprf/batch", "evaluated ${resp.evaluated.size} of ${blinders.size}")
                }
                for ((i, b) in blinders.withIndex()) out += b.finalize(resp.evaluated[i])
            } finally {
                blinders.forEach { it.close() }
            }
        }
        // An own @name is also what a claim and the taken check ask: kept.
        for ((i, t) in terms.withIndex()) if (t is OwnTerm.Username) recentOprf.put(t.value, out[i])
        return out
    }

    /** An own `@name`'s OPRF answer: the one kept from the last 60 s (the
     *  taken check), else evaluated through the batch route. */
    public suspend fun oprfOwnName(name: String): uniffi.lakko_ffi_uniffi.OprfResult =
        recentOprf.get(name) ?: oprfOwnTerms(listOf(OwnTerm.Username(name))).single()

    /** `POST /discovery/name/lookup` (anonymous): the accounts in Everyone
     *  that published the name term whose discovery lookup key is [lookupKey]
     *  (`OprfResult.lookupKey`, never the OPRF output; at most 20). */
    public suspend fun nameLookup(lookupKey: ByteArray): List<PublicAccountHit> {
        val resp = doPostNullableOn404<NameLookupResponse>("/discovery/name/lookup", anonymous = true) {
            contentType(ContentType.Application.Json)
            setBody(NameLookupRequest(oprf_hash_b64 = base64Encode(lookupKey)))
        } ?: return emptyList()
        return resp.results.mapNotNull { r ->
            if (r.kind == "handle") return@mapNotNull null
            val idB64 = r.account_id_b64 ?: return@mapNotNull null
            val id = try {
                base64Decode(idB64).takeIf { id -> id.size == ACCOUNT_ID_LEN }
            } catch (_: IllegalArgumentException) {
                null
            } ?: return@mapNotNull null
            PublicAccountHit(accountId = id, publicRecord = decodeRecordOrNull(r.public_record_b64))
        }
    }

    /**
     * `POST /discovery/lookup` by a username's discovery LOOKUP KEY
     * (`OprfResult.lookupKey`, never the OPRF output): the account in
     * Everyone and its public record, or `null` (404 — unregistered and
     * hidden answer alike). ADR-0060 §5.
     */
    public suspend fun lookupPublic(lookupKey: ByteArray): PublicAccountHit? {
        val resp = lookupRaw(LookupRequest(oprf_hash = base64Encode(lookupKey))) ?: return null
        if (resp.kind == "handle") return null
        val id = base64Decode(resp.account_id ?: return null)
        if (id.size != ACCOUNT_ID_LEN) return null
        return PublicAccountHit(accountId = id, publicRecord = decodeRecordOrNull(resp.public_record_b64))
    }

    // -----------------------------------------------------------------
    // ADR-0087 sections 1 to 4 — @names of groups (and channels)
    // -----------------------------------------------------------------

    /** `POST /discovery/lookup` by a name's lookup key: the sealed record of a
     *  public group or channel holding it, or `null` (a person, a private
     *  name and an unknown one all answer nothing here). */
    public suspend fun lookupHandleRecord(lookupKey: ByteArray): ByteArray? {
        val resp = lookupRaw(LookupRequest(oprf_hash = base64Encode(lookupKey))) ?: return null
        if (resp.kind != "handle") return null
        return decodeRecordOrNull(resp.public_record_b64)
    }

    /** Section 3.3 — whether a name answers a lookup (a person in Everyone,
     *  or a public group or channel): `That @name is taken.` Private names
     *  answer nothing, by design. */
    public suspend fun nameAnswers(name: String): Boolean = recentNameAnswers.getOrAsk(name) {
        val pair = oprfUsername(name)
        lookupRaw(LookupRequest(oprf_hash = base64Encode(pair.lookupKey))) != null
    }

    /** `POST /discovery/name/lookup` keeping only the public groups and
     *  channels: their sealed records. */
    public suspend fun nameLookupHandles(lookupKey: ByteArray): List<ByteArray> {
        val resp = doPostNullableOn404<NameLookupResponse>("/discovery/name/lookup", anonymous = true) {
            contentType(ContentType.Application.Json)
            setBody(NameLookupRequest(oprf_hash_b64 = base64Encode(lookupKey)))
        } ?: return emptyList()
        return resp.results.filter { it.kind == "handle" }.mapNotNull { decodeRecordOrNull(it.public_record_b64) }
    }

    /**
     * One signed handle statement to its route (section 3): `claim` and
     * `replace` spend one `discovery-register` token, `update` / `rekey` /
     * `retire` none. No session. The certificate of a write, or `null` for a
     * retire. A refusal throws [HandleWriteError] with the server's code.
     */
    public suspend fun handleWrite(
        route: HandleRoute,
        statementB64: String,
        record: ByteArray? = null,
        terms: List<ByteArray> = emptyList(),
    ): ByteArray? {
        val endpoint = when (route) {
            HandleRoute.CLAIM -> "/discovery/handle/claim"
            HandleRoute.REPLACE -> "/discovery/handle/replace"
            HandleRoute.REKEY -> "/discovery/handle/rekey"
            HandleRoute.UPDATE, HandleRoute.RETIRE -> "/discovery/handle"
        }
        val body = buildString {
            append("{\"statement_b64\":\"").append(statementB64).append('"')
            if (record != null) append(",\"public_record_b64\":\"").append(base64Encode(record)).append('"')
            if (terms.isNotEmpty()) {
                append(",\"terms_b64\":[")
                append(terms.joinToString(",") { "\"" + base64Encode(it) + "\"" })
                append(']')
            }
            append('}')
        }
        val url = "$baseUrl$endpoint"
        val send: suspend (String?, String?) -> HttpResponse = { name, value ->
            try {
                anonClient.request(url) {
                    method = when (route) {
                        HandleRoute.UPDATE -> io.ktor.http.HttpMethod.Put
                        HandleRoute.RETIRE -> io.ktor.http.HttpMethod.Delete
                        else -> io.ktor.http.HttpMethod.Post
                    }
                    if (name != null && value != null) header(name, value)
                    setBody(io.ktor.http.content.TextContent(body, ContentType.Application.Json))
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                throw DiscoveryNetworkError(endpoint, cause)
            }
        }
        val response = if (route == HandleRoute.CLAIM || route == HandleRoute.REPLACE) {
            anonTokens.sendWithToken(AnonTokenContext.DISCOVERY_REGISTER) { name, value -> send(name, value) }
        } else {
            send(null, null)
        }
        if (!response.status.isSuccess()) {
            val text = response.safeReadBody()
            val code = Regex("\"error\"\\s*:\\s*\"([a-z0-9-]+)\"").find(text)?.groupValues?.get(1)
            throw HandleWriteError(response.status.value, code)
        }
        if (route == HandleRoute.RETIRE || response.status.value == 204) return null
        val text = response.bodyAsText()
        val cert = Regex("\"handle_certificate_b64\"\\s*:\\s*\"([A-Za-z0-9+/=]+)\"").find(text)?.groupValues?.get(1)
            ?: return null
        lastLeaseUntilMs = Regex("\"lease_until_ms\"\\s*:\\s*([0-9]+)").find(text)?.groupValues?.get(1)?.toLongOrNull()
        return base64Decode(cert)
    }

    /** The lease end of the last handle write that answered one (section 3). */
    @Volatile
    public var lastLeaseUntilMs: Long? = null
        private set

    private fun decodeRecordOrNull(b64: String?): ByteArray? = b64?.let {
        try {
            base64Decode(it)
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    // -----------------------------------------------------------------
    // High-level: @channel discovery (G4-5, ADR-0010)
    //
    // The channel namespace reuses the SAME OPRF flow as @username
    // (the `/discovery/oprf` evaluate endpoint is namespace-agnostic
    // — one static key over the blinded handle). Only the second
    // endpoint differs: `/discovery/channel/{register,lookup}`, and
    // the id type is a 16-byte channel_id pseudonym rather than a
    // 16-byte account UUIDv7.
    // -----------------------------------------------------------------

    /**
     * Bind `@channelname` to a 16-byte `channel_id`.
     *
     * Flow: OPRF blind → `POST /discovery/oprf` (a `discovery-lookup`
     * token) → finalize → `POST /discovery/channel/register` (a
     * `discovery-register` token). Identical to
     * [registerUsername] modulo the id type + second endpoint.
     *
     * Handle normalisation (NFKC + lowercase + trim + leading-`@`
     * strip) happens inside [OprfBlinder.blind]. The write is
     * anonymous and rationed by the token (NOT an ownership proof —
     * see `services/discovery/src/channel_names.rs`); no session rides
     * on it.
     *
     * @return `true` once the server bound the handle. A `!ok` reply
     *   or any non-2xx status throws [DiscoveryHttpError] instead —
     *   in particular `status == 409` on a handle collision
     *   (first-come-first-served).
     */
    public suspend fun registerChannelName(
        channelId: ByteArray,
        handle: String,
    ): Boolean {
        if (channelId.size != CHANNEL_ID_LEN) {
            throw DiscoveryClientArgError("channelId must be $CHANNEL_ID_LEN bytes")
        }
        return OprfBlinder.blind(handle).use { blinder ->
            val oprfResp = oprfEvaluate(OprfRequest(blinded = blinder.blindedB64()))

            val lookupKeyB64 = base64Encode(blinder.finalize(oprfResp.evaluated).lookupKey)

            val resp = channelRegister(
                ChannelRegisterRequest(
                    oprf_hash = lookupKeyB64,
                    channel_id = base64Encode(channelId),
                ),
            )
            if (!resp.ok) {
                throw DiscoveryHttpError(
                    status = 200,
                    endpoint = "/discovery/channel/register",
                    body = "server reported ok=false",
                )
            }
            true
        }
    }

    /**
     * Resolve `@channelname` to its `channel_id` as a 32-char
     * lowercase hex string, or `null` when no channel has claimed
     * the handle (the server's 404 not-found path).
     *
     * Same flow as [registerChannelName] modulo the second
     * endpoint: OPRF (one token) → `POST /discovery/channel/lookup`.
     * Unauthenticated (the lookup path is open); anonymous-safe.
     */
    public suspend fun lookupChannel(handle: String): String? {
        // ADR-0087 section 1 — one namespace: a channel's @name is a handle,
        // found by its lookup key and opened from its sealed record.
        val name = me.lakko.app.chat.canonicalHandleInput(handle)
        if (!me.lakko.app.chat.handleWellFormed(name)) return null
        val pair = oprfUsername(name)
        val record = lookupHandleRecord(pair.lookupKey) ?: return null
        val found = me.lakko.app.chat.openFoundConversation(record, pair.output) ?: return null
        if (found.kind != uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.CHANNEL || found.id.size != CHANNEL_ID_LEN) return null
        return bytesToHexLower(found.id)
    }

    /**
     * Upload (or replace) the user's MLS KeyPackage. Drives one
     * `POST /discovery/keypackage` (session-gated).
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
        deviceId: ByteArray,
    ) {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (deviceId.size != DEVICE_ID_LEN) {
            throw DiscoveryClientArgError("deviceId must be $DEVICE_ID_LEN bytes")
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

        val resp = keypackageUpload(
            KeyPackageUploadRequest(
                account_id = base64Encode(accountId),
                device_id = base64Encode(deviceId),
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
     * Resolve `accountId` to every device's KeyPackage bytes (TLS-encoded
     * MLS), oldest device first (ADR-0050). Returns `null` when the account
     * has published none. A caller adding the account adds ALL of them; the
     * device binding inside each leaf is what the core verifies.
     */
    public suspend fun fetchKeyPackages(accountId: ByteArray): List<DeviceKeyPackage>? {
        val raw = keypackageFetch(accountId) ?: return null
        return raw.keypackages.map {
            DeviceKeyPackage(
                deviceId = base64Decode(it.device_id),
                keyPackage = base64Decode(it.keypackage),
            )
        }
    }

    /**
     * `DELETE /discovery/keypackage/device/{device_id_b64url}` — remove one of
     * the authenticated account's device KeyPackages (ADR-0050 device removal).
     * Bearer-auth; idempotent (204).
     *
     * ADR-0074 section 2.2: the session's OWN device row needs nothing more.
     * ANOTHER device's row needs [statement], the 138-byte `device-revoke`
     * account statement for that id, sent as the `x-lakko-account-statement`
     * header ([statementHeaderName]; base64url without padding); without it
     * the server answers `403 account-key-required`. Every delete tombstones
     * the id for 30 days, a device's own included.
     */
    public suspend fun deleteKeyPackage(
        deviceId: ByteArray,
        statement: ByteArray? = null,
        statementHeaderName: String = ACCOUNT_STATEMENT_HEADER,
    ) {
        if (deviceId.size != DEVICE_ID_LEN) {
            throw DiscoveryClientArgError("deviceId must be $DEVICE_ID_LEN bytes")
        }
        val endpoint = "/discovery/keypackage/device/${base64UrlEncodeNoPad(deviceId)}"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.delete(url) {
                sessionToken?.let {
                    header(io.ktor.http.HttpHeaders.Authorization, "Bearer $it")
                }
                if (statement != null) {
                    header(statementHeaderName, me.lakko.app.auth.base64UrlEncodeNoPad(statement))
                }
            }
        } catch (cause: Throwable) {
            throw DiscoveryNetworkError("$endpoint (DELETE)", cause)
        }
        if (response.status.isSuccess()) return
        throw DiscoveryHttpError(
            status = response.status.value,
            endpoint = "$endpoint (DELETE)",
            body = response.safeReadBody(),
        )
    }

    /**
     * Upload a freshly-composed [InviteCapability] blob and
     * receive the server-assigned `blobId` (a random 6-byte
     * ledger id, ADR-0028 amendment of 2026-09-27). Drives one
     * `POST /discovery/invite` (session-gated). NOT idempotent: the
     * same bytes uploaded twice get two ids.
     *
     * Feed `capabilityBytes` straight from
     * [uniffi.lakko_ffi_uniffi.composeInviteCapability]. The
     * server runs `decode_and_verify` server-side, so a junk
     * upload surfaces as
     * `DiscoveryHttpError(400, "/discovery/invite", …)`.
     *
     * Returns the 6-byte `blobId`, ready to substitute into a
     * `https://<host>/i/<slug>` pointer URL via
     * [base64UrlEncodeNoPad].
     */
    public suspend fun uploadInvite(upload: SealedInviteUpload): ByteArray {
        checkSealedUpload(upload, MAX_CAPABILITY_BYTES)
        val resp = inviteUpload(
            InviteUploadRequest(
                sealed = base64Encode(upload.sealed),
                revocation_pubkey = base64Encode(upload.revocationPubkey),
                expiry_ms = upload.expiryMs,
                max_uses = upload.maxUses,
            ),
        )
        // Server returns the 8-character base64url id; decode it
        // back to the raw 6 bytes.
        return uploadedBlobId(resp.blob_id, "/discovery/invite")
    }

    /**
     * Fetch a previously-uploaded `InviteCapability` by its
     * ledger id [blobId]. Returns `null` on 404 (server
     * folds expired blobs into the same status), the raw
     * capability bytes on 200 (the caller drives
     * [uniffi.lakko_ffi_uniffi.InviteCapability.decodeAndVerify]
     * to re-verify locally so a malicious server cannot serve
     * substituted bytes).
     */
    public suspend fun fetchInvite(blobId: ByteArray): ByteArray? = fetchInviteRow(blobId)?.sealed

    /** [fetchInvite] with whether every use of the link is spent. */
    public suspend fun fetchInviteRow(blobId: ByteArray): FetchedInvite? {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val raw = inviteFetch(blobId) ?: return null
        return FetchedInvite(base64Decode(raw.sealed), raw.used_up)
    }

    /**
     * Atomically bump the consume counter for the invite at
     * [blobId] with per-joiner dedup. Returns a discriminated
     * [ConsumeInviteResult] so the joiner UI can branch on the
     * four observable outcomes without exception-walking the
     * underlying [DiscoveryHttpError] hierarchy.
     *
     * Draws one `invite-consume` token + one
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        if (joinerAccountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("joinerAccountId must be $ACCOUNT_ID_LEN bytes")
        }

        val joinerTag = computeJoinerInviteTag(capabilityBytes, joinerAccountId)
        val slug = base64UrlEncodeNoPad(blobId)

        val endpoint = "/discovery/invite/$slug/consume"
        val response: HttpResponse = consumeRequest(endpoint, joinerTag)
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
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
     * receive the server-assigned 6-byte `blobId` (a fresh
     * random ledger id per upload). Session-gated
     * `POST /discovery/call-invite`.
     *
     * Sibling of [uploadInvite]; the client-side ceiling is
     * [MAX_CALL_CAPABILITY_BYTES] (8 KiB, 4× contact-invite)
     * because the call-invite capability embeds a one-time
     * MLS KeyPackage.
     *
     * Returns the 6-byte `blobId`, ready to substitute into a
     * `https://<host>/c/<slug>` pointer URL via
     * [base64UrlEncodeNoPad].
     */
    public suspend fun uploadCallInvite(upload: SealedInviteUpload): ByteArray {
        checkSealedUpload(upload, MAX_CALL_CAPABILITY_BYTES)
        val resp = callInviteUpload(
            CallInviteUploadRequest(
                sealed = base64Encode(upload.sealed),
                revocation_pubkey = base64Encode(upload.revocationPubkey),
                expiry_ms = upload.expiryMs,
                max_uses = upload.maxUses,
            ),
        )
        return uploadedBlobId(resp.blob_id, "/discovery/call-invite")
    }

    /**
     * Fetch a previously-uploaded `CallInviteCapability` by its
     * ledger id [blobId]. Returns `null` on 404 (server
     * folds expired blobs into the same status), raw capability
     * bytes on 200 (the caller drives
     * [uniffi.lakko_ffi_uniffi.CallInviteCapability.decodeAndVerify]
     * to re-verify locally so a malicious server cannot serve
     * substituted bytes).
     */
    public suspend fun fetchCallInvite(blobId: ByteArray): ByteArray? {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val raw = callInviteFetch(blobId) ?: return null
        return base64Decode(raw.sealed)
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        if (joinerAccountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("joinerAccountId must be $ACCOUNT_ID_LEN bytes")
        }

        val joinerTag = computeJoinerInviteTag(capabilityBytes, joinerAccountId)
        val slug = base64UrlEncodeNoPad(blobId)

        val endpoint = "/discovery/call-invite/$slug/consume"
        val response: HttpResponse = consumeRequest(endpoint, joinerTag)
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
     * Upload a freshly-composed `GroupInviteCapability` blob and receive the
     * server-assigned 6-byte `blobId` (a fresh random ledger id per upload).
     * Session-gated `POST /discovery/group-invite`.
     *
     * `maxUses` is a SOFT anti-spam throttle (NOT part of the signed capability,
     * NOT an access control — the admin approval gate is the security
     * boundary): `0` = single-use, `0xFFFFFFFF` = unlimited.
     *
     * Returns the 6-byte `blobId`, ready to substitute into a
     * `https://<host>/g/<slug>` pointer URL via [base64UrlEncodeNoPad].
     */
    public suspend fun uploadGroupInvite(upload: SealedInviteUpload): ByteArray {
        // No client ceiling: the sealed preview makes the width depend on
        // the names; the server enforces the codec's exact bound.
        checkSealedUpload(upload, capabilityCeiling = null)
        val resp = groupInviteUpload(
            GroupInviteUploadRequest(
                sealed = base64Encode(upload.sealed),
                revocation_pubkey = base64Encode(upload.revocationPubkey),
                expiry_ms = upload.expiryMs,
                max_uses = upload.maxUses,
            ),
        )
        return uploadedBlobId(resp.blob_id, "/discovery/group-invite")
    }

    /** Fail fast before the network on an upload the server refuses anyway:
     *  an empty sealed blob, one past the capability ceiling plus the seal's
     *  overhead ([SEAL_OVERHEAD_MARGIN], a loose bound), or a revocation key
     *  that is not 32 bytes. */
    private fun checkSealedUpload(upload: SealedInviteUpload, capabilityCeiling: Int?) {
        if (upload.sealed.isEmpty()) {
            throw DiscoveryClientArgError("sealed must be non-empty")
        }
        val ceiling = capabilityCeiling?.let { it + SEAL_OVERHEAD_MARGIN }
        if (ceiling != null && upload.sealed.size > ceiling) {
            throw DiscoveryClientArgError(
                "sealed exceeds the $ceiling-byte ceiling (got ${upload.sealed.size} bytes)",
            )
        }
        if (upload.revocationPubkey.size != SIGNING_SEED_LEN) {
            throw DiscoveryClientArgError("revocationPubkey must be $SIGNING_SEED_LEN bytes")
        }
    }

    /**
     * Fetch a previously-uploaded `GroupInviteCapability` by its
     * ledger id [blobId]. Returns `null` on 404 (the server folds
     * expired blobs into the same status), raw capability bytes on 200 (the
     * caller drives
     * [uniffi.lakko_ffi_uniffi.GroupInviteCapability.decodeAndVerify] to
     * re-verify locally so a malicious server cannot serve substituted bytes).
     */
    public suspend fun fetchGroupInvite(blobId: ByteArray): ByteArray? {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        val raw = groupInviteFetch(blobId) ?: return null
        return base64Decode(raw.sealed)
    }

    /**
     * Atomically bump the consume counter for the group-invite at [blobId] with
     * per-joiner dedup. Returns a discriminated [ConsumeInviteResult]. The
     * consume is anti-spam defence-in-depth — the admin approval gate (ADR-0027)
     * remains the security boundary.
     *
     * Reuses the same `computeJoinerInviteTag(...)` helper as the contact- and
     * call-invite flows — the salt is per-blob, so tags don't collide across
     * invite kinds.
     */
    public suspend fun consumeGroupInvite(
        blobId: ByteArray,
        capabilityBytes: ByteArray,
        joinerAccountId: ByteArray,
    ): ConsumeInviteResult {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        if (joinerAccountId.size != ACCOUNT_ID_LEN) {
            throw DiscoveryClientArgError("joinerAccountId must be $ACCOUNT_ID_LEN bytes")
        }

        val joinerTag = computeJoinerInviteTag(capabilityBytes, joinerAccountId)
        val slug = base64UrlEncodeNoPad(blobId)

        val endpoint = "/discovery/group-invite/$slug/consume"
        val response: HttpResponse = consumeRequest(endpoint, joinerTag)
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

    /** ADR-0028 revoke — wire-level `DELETE /discovery/group-invite/{blob_id}`.
     *  Sibling of [callInviteRevoke]; the body carries the issuer-signed
     *  revocation. The high-level [revokeGroupInvite] wrapper drives the sign. */
    public suspend fun groupInviteRevoke(blobIdSlug: String, req: InviteRevokeRequest) {
        val endpoint = "/discovery/group-invite/$blobIdSlug"
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

    /**
     * ADR-0028 revoke — issuer-signed delete of a group-invite blob. Sibling of
     * [revokeCallInvite]: signs the group-invite revoke canonical bytes with
     * [signingSeed] (the issuer's ed25519 seed) and DELETEs the ledger row; the
     * server verifies against the stored capability's issuer pubkey. Returns a
     * discriminated [RevokeInviteResult]; 404 folds into `NotFoundOrNotOwner`.
     */
    public suspend fun revokeGroupInvite(
        blobId: ByteArray,
        signingSeed: ByteArray,
    ): RevokeInviteResult {
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
        }
        if (signingSeed.size != SIGNING_SEED_LEN) {
            throw DiscoveryClientArgError("signingSeed must be $SIGNING_SEED_LEN bytes")
        }
        val signedAtMs = System.currentTimeMillis()
        val signature = signGroupInviteRevocation(signingSeed, blobId, signedAtMs.toULong())
        val slug = base64UrlEncodeNoPad(blobId)
        return try {
            groupInviteRevoke(
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
        if (blobId.size != blobIdLen) {
            throw DiscoveryClientArgError("blobId must be $blobIdLen bytes")
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
     * Upload (or replace) the identity bundle for [accountId].
     * Session-gated `POST /discovery/identity`.
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
     *
     * ADR-0036 (G-30 finding 2): [genesisAttest], when non-null, is the
     * raw auth-attested genesis attestation minted for a genesis /
     * fresh-start publish. It rides onto [IdentityUploadRequest.genesis_attest]
     * (standard base64) alongside [leafAuth]; the server forwards it
     * opaquely to the transparency append gate, which prevents first-entry
     * squatting. `null` on a same-key re-publish (or when the mint was
     * unavailable during rollout — additive, flag-gated server-side).
     */
    public suspend fun uploadIdentityBundle(
        accountId: ByteArray,
        signaturePubkey: ByteArray,
        x25519Pubkey: ByteArray,
        supersessionProofB64: String? = null,
        leafAuth: ByteArray? = null,
        genesisAttest: ByteArray? = null,
        /** ADR-0073: the raw 145-byte key-reset attestation of a reset's
         *  publish; `null` for every other upload. The server refuses an
         *  unattested, unproven key change (`409
         *  identity-change-needs-attestation`), so a changed key is never
         *  uploaded without it. */
        keyResetAttest: ByteArray? = null,
        /** ADR-0074 section 3.7: the raw 201-byte conversation link of a
         *  rotation's publish; `null` for every other upload. */
        conversationLink: ByteArray? = null,
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

        val resp = identityUpload(
            IdentityUploadRequest(
                account_id = base64Encode(accountId),
                signature_pubkey = base64Encode(signaturePubkey),
                x25519_pubkey = base64Encode(x25519Pubkey),
                supersession_proof = supersessionProofB64,
                leaf_auth = leafAuth?.let { base64Encode(it) },
                // ADR-0036 — standard-alphabet base64 (mirrors `leaf_auth`;
                // the discovery upload decodes it with the STANDARD engine).
                genesis_attest = genesisAttest?.let { base64Encode(it) },
                // ADR-0073 - standard base64 of the 145 bytes, like the two
                // sidecars above (re-encoded from the server's base64url).
                key_reset_attest = keyResetAttest?.let { base64Encode(it) },
                conversation_link = conversationLink?.let { base64Encode(it) },
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
     * One ledger consume on the anonymous client with an `invite-consume`
     * token and one retry on a token refusal. No session: the joiner is named
     * only by the server-blind dedup tag.
     */
    private suspend fun consumeRequest(endpoint: String, joinerTag: ByteArray): HttpResponse {
        val url = "$baseUrl$endpoint"
        return anonTokens.sendWithToken(AnonTokenContext.INVITE_CONSUME) { name, value ->
            try {
                anonClient.post(url) {
                    header(name, value)
                    contentType(ContentType.Application.Json)
                    setBody(InviteConsumeRequest(joiner_tag = base64Encode(joinerTag)))
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                throw DiscoveryNetworkError(endpoint, cause)
            }
        }
    }

    /**
     * One POST on a token route: the anonymous client, the token header, one
     * retry on a token refusal, never `Authorization`.
     */
    private suspend inline fun <reified Res : Any> tokenPost(
        endpoint: String,
        context: AnonTokenContext,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = anonTokens.sendWithToken(context) { name, value ->
            try {
                anonClient.post(url) {
                    header(name, value)
                    configure()
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                throw DiscoveryNetworkError(endpoint, cause)
            }
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
            val body = response.safeReadBody()
            throw DiscoveryHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = body,
                // The wait a `429` states: the body's `retry_after_secs`,
                // else the `Retry-After` header (whole seconds).
                retryAfterSecs = me.lakko.app.anon.longFieldOf(body, "retry_after_secs")
                    ?: response.headers[io.ktor.http.HttpHeaders.RetryAfter]?.trim()?.toLongOrNull(),
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
        anonymous: Boolean = false,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res? {
        val url = "$baseUrl$endpoint"
        // The lookups that follow an OPRF evaluation go out anonymously: a
        // session there would name who looked up whom.
        val authHeader = if (anonymous) null else sessionToken?.let { "Bearer $it" }
        val http = if (anonymous) anonClient else client
        val response: HttpResponse = try {
            http.post(url) {
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
        /** Section 6.1 — OPRF answers by username, kept 60 s, process-wide
         *  (every client instance asks the same server key). */
        private val recentOprf = RecentAnswers<uniffi.lakko_ffi_uniffi.OprfResult>()

        /** Section 3.3 — whether a name answered a lookup, kept 60 s. */
        private val recentNameAnswers = RecentAnswers<Boolean>()

        /** ADR-0074 - the header a statement rides in on a route with no
         *  JSON body. The native core names it (`accountStatementHeaderName`);
         *  this is the same value for code that runs without the core. */
        public const val ACCOUNT_STATEMENT_HEADER: String = "x-lakko-account-statement"

        /**
         * Default discovery service URL — `<LAKKO_API_BASE_URL>/discovery`
         * ([ApiEndpoints]), or the Android emulator loopback alias +
         * the dev `lakko-svc-discovery` bind (`http://10.0.2.2:8084`)
         * when the build carries no API base URL.
         */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.DISCOVERY)

        /** Raw 16-byte UUIDv7. */
        private const val ACCOUNT_ID_LEN: Int = 16
        private const val DEVICE_ID_LEN: Int = 32

        /** Raw 16-byte channel_id pseudonym (G4-5, ADR-0010). */
        private const val CHANNEL_ID_LEN: Int = 16

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

        /** Room above a capability ceiling for the link seal (kind + version
         *  bytes, nonce, tag). A loose client-side bound; the server enforces
         *  its own exact ceiling. */
        private const val SEAL_OVERHEAD_MARGIN: Int = 64

        /** Ed25519 signing seed length, mirrors
         *  `me.lakko.app.auth` constants. */
        private const val SIGNING_SEED_LEN: Int = 32
    }
}

/** Lowercase hex-encode (no separators). Used to render a
 *  resolved `channel_id` for the channels UI, which speaks hex on
 *  every `services/channels` REST route (G4-5, ADR-0010). */
private fun bytesToHexLower(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(Character.forDigit(v ushr 4, 16))
        sb.append(Character.forDigit(v and 0x0F, 16))
    }
    return sb.toString()
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
/** One device of an account as the directory serves it (ADR-0050). */
public class DeviceKeyPackage(
    /** The device leaf's 32-byte Ed25519 signature key. */
    public val deviceId: ByteArray,
    /** TLS-encoded MLS KeyPackage bytes. */
    public val keyPackage: ByteArray,
)

public data class RegisterResult(
    /** The canonical form actually claimed (after NFKC + lowercase
     *  + trim + leading-`@` strip). Surface to the UI so the user
     *  sees what handle is now bound to their account. */
    val normalisedUsername: String,
    /** The username certificate the register call returned, if any. */
    val usernameCertificate: ByteArray? = null,
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

/** One row of the owner's "Invite links" list. Since ADR-0078 section 10
 *  it is built from this device's own records
 *  ([me.lakko.app.contacts.OwnLinkStore]), not from a server query: no
 *  ledger row names its owner. `usedCount` is therefore not known (0). */
public data class ActiveInvite(
    /** URL-safe base64 (no padding) of the 6-byte blob_id —
     *  same `/i/<slug>` segment that appears in the share URL. */
    val blobIdSlug: String,
    /** UNIX millis the capability expires at. `0` means never
     *  expires (the `NEVER_EXPIRES` sentinel). */
    val expiryMs: Long,
    /** Wire-format `max_uses` sentinel: `0` = SINGLE_USE,
     *  `0xFFFFFFFF` = unlimited, any other N = N consumes
     *  allowed. */
    val maxUses: Long,
    /** Consumes, when known; 0 for a row from this device's records. */
    val usedCount: Long,
    /** Owner-supplied free-text label; empty string when not
     *  set. */
    val label: String,
    /** `true` if the capability flags the link as a referral
     *  signup-deep-link (Phase 1.9b4). */
    val bootstrapReferral: Boolean,
    /** When this device minted the link (UNIX ms). */
    val createdAtMs: Long,
)

/** Result of `ContactsRepository.listActiveInvites`. */
public sealed interface ListInvitesResult {
    /** The links this device minted and still keeps (possibly empty). */
    public data class List(val invites: kotlin.collections.List<ActiveInvite>) :
        ListInvitesResult
    /** Kept for the callers; the local list never answers it. */
    public data object NotFoundOrNotOwner : ListInvitesResult
}

/** ADR-0060 §5 — one account in Everyone a lookup answered with, and its
 *  public record (`null` when discovery sent none). */
public class PublicAccountHit(
    /** Raw 16-byte account id. */
    public val accountId: ByteArray,
    /** The opaque public record, opened with the OPRF OUTPUT of the round
     *  that found the hit (`openPublicRecord`). */
    public val publicRecord: ByteArray?,
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

/** One term of an own name to evaluate: an `@name` / username, or a name
 *  search term. */
public sealed class OwnTerm {
    public abstract val value: String

    public data class Username(override val value: String) : OwnTerm()

    public data class NameTerm(override val value: String) : OwnTerm()
}

/** The most elements `POST /discovery/oprf/batch` takes: `maxHandleTerms()`
 *  + 1, room for the old @name beside the new one and its name terms in a
 *  replace. A longer list goes in several batches; no term is dropped. */
public const val OPRF_BATCH_MAX: Int = 66

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
    /** The body's `error` code (`key-generation-not-newer`,
     *  `identity-change-needs-attestation`, ...), when it carried one. */
    public val code: String? = me.lakko.app.anon.errorCodeOf(body),
    /** The wait a `429` states (`retry_after_secs`), when it carried one. */
    public val retryAfterSecs: Long? = me.lakko.app.anon.longFieldOf(body, "retry_after_secs"),
) : RuntimeException("discovery $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). Distinct from
 *  [DiscoveryHttpError] so the UI can suggest "check connection". */
public class DiscoveryNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("discovery $endpoint network error: $cause", cause)

/** A ledger row as [DiscoveryClient.fetchInviteRow] answers it. */
public class FetchedInvite(public val sealed: ByteArray, public val usedUp: Boolean)

/** ADR-0087 section 3 — the handle routes. */
public enum class HandleRoute { CLAIM, UPDATE, REPLACE, REKEY, RETIRE }

/** A refused handle write, with the server's error code (`name-taken`, ...),
 *  mapped to copy by the caller, never shown as is. */
public class HandleWriteError(public val status: Int, public val code: String?) :
    RuntimeException("handle write refused: $status ${code ?: ""}")
