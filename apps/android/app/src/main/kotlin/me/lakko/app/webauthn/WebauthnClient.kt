// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import java.security.MessageDigest

/**
 * Ktor-based client for `lakko-svc-auth`'s `/webauthn/`
 * endpoint family (Phase 2a2e). Path-glob `*` dropped from
 * the prose so the KDoc block doesn't terminate prematurely
 * — Kotlin lexer reads `*` + `/` inside a `/** ... */` doc
 * comment as the closing token.
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/webauthn-client.ts`
 *   * `apps/ios/Lakko/Webauthn/WebauthnClient.swift`
 *
 * ## API surface
 *
 *   * [startRegistration] / [finishRegistration] — pair of calls
 *     wrapped by [WebauthnRegistration.register] (which drives
 *     [androidx.credentials.CredentialManager] between the two).
 *   * [listCredentials] / [deleteCredential] — bearer-auth CRUD
 *     for the user's stored passkeys.
 *   * [startAuthentication] / [finishAuthentication] — anonymous
 *     login pair. Wired but not consumed by the UI yet — see
 *     the 2a2c2 cache-exportKey sub-sprint.
 *
 * ## Wire-format note
 *
 * Android's `CredentialManager` API takes / returns the WebAuthn
 * JSON form directly (`requestJson` / `registrationResponseJson`
 * / `authenticationResponseJson`), so the client carries
 * [JsonElement] for the options + response slots and lets the
 * platform layer round-trip the bytes. iOS and web pre-decode /
 * pre-encode by hand because their authenticator surfaces don't
 * accept raw JSON.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8081` — Android-emulator loopback alias, port
 * `8081` matching `lakko-svc-auth`'s dev bind.
 */
public class WebauthnClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    /** Lenient JSON: webauthn-rs may add fields we don't model
     *  yet; we ignore unknown keys instead of failing the
     *  decode. */
    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) { install(ContentNegotiation) { json(json) } }
    } else {
        HttpClient(OkHttp) { install(ContentNegotiation) { json(json) } }
    }

    /** Bearer token bound to the four authenticated routes
     *  (`/register/`-prefixed, `/credentials`, `/credentials/:id`).
     *  The anonymous `/auth/`-prefixed routes ignore it.
     *  Path-glob `*` dropped here too — same KDoc-block lexer
     *  gotcha as the file header. */
    private var sessionToken: String? = null

    public fun setSessionToken(token: String?) {
        this.sessionToken = token
    }

    override fun close() {
        client.close()
    }

    /** Expose the configured JSON for the
     *  [WebauthnRegistration] helper — it needs the same lenient
     *  config to round-trip the `requestJson` string the
     *  Credential Manager expects. */
    internal val jsonCodec: Json get() = json

    // -----------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------

    public suspend fun startRegistration(
        userName: String,
        nickname: String,
    ): RegisterStartResponse {
        val req = RegisterStartRequest(userName = userName, nickname = nickname)
        val response = client.post("$baseUrl/webauthn/register/start") {
            contentType(ContentType.Application.Json)
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            setBody(req)
        }
        ensure2xx(response, "/webauthn/register/start")
        return response.body()
    }

    public suspend fun finishRegistration(
        stateId: String,
        nickname: String,
        credential: JsonElement,
    ): RegisterFinishResponse {
        val req = RegisterFinishRequest(
            stateId = stateId,
            nickname = nickname,
            credential = credential,
        )
        val response = client.post("$baseUrl/webauthn/register/finish") {
            contentType(ContentType.Application.Json)
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            setBody(req)
        }
        ensure2xx(response, "/webauthn/register/finish")
        return response.body()
    }

    // -----------------------------------------------------------------
    // Authentication
    // -----------------------------------------------------------------

    public suspend fun startAuthentication(
        accountIdB64: String?,
    ): AuthStartResponse {
        val req = AuthStartRequest(accountIdB64 = accountIdB64)
        val response = client.post("$baseUrl/webauthn/auth/start") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }
        ensure2xx(response, "/webauthn/auth/start")
        return response.body()
    }

    public suspend fun finishAuthentication(
        stateId: String,
        credential: JsonElement,
    ): AuthFinishResponse {
        val req = AuthFinishRequest(stateId = stateId, credential = credential)
        val response = client.post("$baseUrl/webauthn/auth/finish") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }
        ensure2xx(response, "/webauthn/auth/finish")
        return response.body()
    }

    /**
     * Phase 2a2c2-android — drive the full passkey-login round
     * in one call. Mirrors `WebauthnClient.loginWithPasskey` on
     * web + iOS. Discoverable-credential flow is the default
     * (no `accountIdB64` passed up); the Credential Manager
     * picks any passkey it has for the lakko relying-party id.
     *
     * Returns `null` when the user cancelled the sheet. Throws
     * on any other failure mode ([WebauthnHttpError], etc.).
     */
    public suspend fun loginWithPasskey(
        context: android.content.Context,
        registration: WebauthnRegistration = WebauthnRegistration(),
    ): PasskeyLoginResult? {
        val start = startAuthentication(accountIdB64 = null)
        val publicKey = start.options.publicKeyField()
        return when (val outcome = registration.authenticate(context, publicKey)) {
            is WebauthnRegistration.Outcome.Success -> {
                val finish = finishAuthentication(
                    stateId = start.stateId,
                    credential = outcome.value,
                )
                PasskeyLoginResult(
                    accountIdB64 = finish.accountIdB64,
                    sessionToken = finish.sessionToken,
                    sessionExpiresAtMs = finish.sessionExpiresAtMs,
                )
            }
            WebauthnRegistration.Outcome.Cancelled -> null
            is WebauthnRegistration.Outcome.Failed -> throw outcome.cause
        }
    }

    // -----------------------------------------------------------------
    // Action binding (G-13, ADR-0014)
    // -----------------------------------------------------------------

    public suspend fun startAction(
        actionHashB64: String,
    ): ActionStartResponse {
        val req = ActionStartRequest(actionHashB64 = actionHashB64)
        val response = client.post("$baseUrl/auth/action/start") {
            contentType(ContentType.Application.Json)
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            setBody(req)
        }
        ensure2xx(response, "/auth/action/start")
        return response.body()
    }

    public suspend fun finishAction(
        stateId: String,
        actionHashB64: String,
        credential: JsonElement,
    ): ActionFinishResponse {
        val req = ActionFinishRequest(
            stateId = stateId,
            actionHashB64 = actionHashB64,
            credential = credential,
        )
        val response = client.post("$baseUrl/auth/action/finish") {
            contentType(ContentType.Application.Json)
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            setBody(req)
        }
        ensure2xx(response, "/auth/action/finish")
        return response.body()
    }

    /**
     * Drive the action-binding ceremony (G-13, ADR-0014):
     *
     *   1. `POST /auth/action/start` with the opaque
     *      [actionHashB64]. The server derives
     *      `challenge = H(action_hash || nonce)`, returns
     *      `(state_id, options)`.
     *   2. [WebauthnRegistration.authenticate] drives Credential
     *      Manager's `getCredential` — the user performs the
     *      passkey gesture, signing over the action-bound
     *      challenge.
     *   3. `POST /auth/action/finish` with the same
     *      [actionHashB64] and the asserted credential. Resolves
     *      only if the server verifies the signature attests to
     *      this action.
     *
     * Bearer-auth required ([setSessionToken]). Mirror of
     * `WebauthnClient.assertAction` on web + iOS.
     *
     * The ceremony is the existing passkey-LOGIN (assertion)
     * flow with two differences: the `/auth/action/{start,finish}`
     * endpoints, and an extra `action_hash_b64` field in both
     * request bodies.
     *
     * Throws on every non-success outcome so the caller can use
     * it as a fail-closed gate: a [WebauthnUserCancelledError]
     * when the user dismisses the sheet, the underlying cause on
     * any authenticator failure, or a [WebauthnHttpError] on a
     * server hiccup (in particular HTTP 404 when the account has
     * no passkey — callers that want a conditional gate branch on
     * that status).
     */
    public suspend fun assertAction(
        context: android.content.Context,
        actionHashB64: String,
        registration: WebauthnRegistration = WebauthnRegistration(),
    ): ActionFinishResponse {
        val start = startAction(actionHashB64)
        val publicKey = start.options.publicKeyField()
        return when (val outcome = registration.authenticate(context, publicKey)) {
            is WebauthnRegistration.Outcome.Success -> finishAction(
                stateId = start.stateId,
                actionHashB64 = actionHashB64,
                credential = outcome.value,
            )
            WebauthnRegistration.Outcome.Cancelled ->
                throw WebauthnUserCancelledError()
            is WebauthnRegistration.Outcome.Failed -> throw outcome.cause
        }
    }

    // -----------------------------------------------------------------
    // Credential management
    // -----------------------------------------------------------------

    public suspend fun listCredentials(): ListCredentialsResponse {
        val response = client.get("$baseUrl/webauthn/credentials") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
        }
        ensure2xx(response, "/webauthn/credentials")
        return response.body()
    }

    /** A3 (GDPR portability) — `GET /account/export`. Bearer-auth; returns what
     *  `services/auth` holds about the account (metadata + auth-factor
     *  existence). */
    public suspend fun exportAccount(): AccountExport {
        val response = client.get("$baseUrl/account/export") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
        }
        ensure2xx(response, "/account/export")
        return response.body()
    }

    public suspend fun deleteCredential(credentialIdB64: String) {
        val endpoint = "/webauthn/credentials/$credentialIdB64"
        val response = client.delete("$baseUrl$endpoint") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
        }
        // 404 is idempotent — already deleted.
        if (response.status == HttpStatusCode.NotFound) return
        ensure2xx(response, endpoint)
    }

    /** A3 (GDPR erasure) — `DELETE /account`. Deletes the whole account on
     *  `services/auth` (FK-cascade over all auth state). Bearer-auth; when the
     *  account has a passkey the server answers 403 unless a fresh action grant
     *  was minted, so the caller runs [assertAction] with
     *  [actionHashForDeleteAccount] first and retries — the 403 surfaces here as
     *  a [WebauthnHttpError]. */
    public suspend fun deleteAccount() {
        val endpoint = "/account"
        val response = client.delete("$baseUrl$endpoint") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
        }
        ensure2xx(response, endpoint)
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private suspend fun ensure2xx(response: HttpResponse, endpoint: String) {
        if (response.status.value in 200..299) return
        val body = response.safeReadBody()
        throw WebauthnHttpError(
            status = response.status.value,
            endpoint = endpoint,
            body = body,
        )
    }

    public companion object {
        /** Android-emulator loopback alias for `lakko-svc-auth`. */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8081"
    }
}

// -----------------------------------------------------------------
// Wire shapes
// -----------------------------------------------------------------

@Serializable
public data class RegisterStartRequest(
    @SerialName("user_name") public val userName: String,
    public val nickname: String,
)

@Serializable
public data class RegisterStartResponse(
    @SerialName("state_id") public val stateId: String,
    public val nickname: String,
    /** `CreationChallengeResponse` from webauthn-rs. Carried as
     *  an opaque [JsonElement] because the Android Credential
     *  Manager API takes the JSON form directly as a string —
     *  no need to decode into a typed tree. */
    public val options: JsonElement,
)

@Serializable
public data class RegisterFinishRequest(
    @SerialName("state_id") public val stateId: String,
    public val nickname: String,
    /** `registrationResponseJson` from Credential Manager,
     *  parsed into JSON. webauthn-rs accepts the same shape on
     *  the wire. */
    public val credential: JsonElement,
)

@Serializable
public data class RegisterFinishResponse(
    @SerialName("credential_id_b64") public val credentialIdB64: String,
)

@Serializable
public data class AuthStartRequest(
    @SerialName("account_id_b64") public val accountIdB64: String?,
)

@Serializable
public data class AuthStartResponse(
    @SerialName("state_id") public val stateId: String,
    public val discoverable: Boolean,
    public val options: JsonElement,
)

@Serializable
public data class AuthFinishRequest(
    @SerialName("state_id") public val stateId: String,
    public val credential: JsonElement,
)

@Serializable
public data class AuthFinishResponse(
    @SerialName("account_id_b64") public val accountIdB64: String,
    @SerialName("session_token") public val sessionToken: String,
    @SerialName("session_expires_at_ms") public val sessionExpiresAtMs: Long,
)

// -----------------------------------------------------------------
// Action binding wire shapes (G-13, ADR-0014)
// -----------------------------------------------------------------

@Serializable
public data class ActionStartRequest(
    /** URL-safe base64 of the 32-byte action hash. */
    @SerialName("action_hash_b64") public val actionHashB64: String,
)

@Serializable
public data class ActionStartResponse(
    @SerialName("state_id") public val stateId: String,
    /** `RequestChallengeResponse` from webauthn-rs — same shape
     *  as [AuthStartResponse.options]. Carried as an opaque
     *  [JsonElement] because the Credential Manager API takes the
     *  JSON form directly. */
    public val options: JsonElement,
)

@Serializable
public data class ActionFinishRequest(
    @SerialName("state_id") public val stateId: String,
    @SerialName("action_hash_b64") public val actionHashB64: String,
    /** `authenticationResponseJson` from Credential Manager,
     *  parsed into JSON — identical to [AuthFinishRequest.credential]. */
    public val credential: JsonElement,
)

@Serializable
public data class ActionFinishResponse(
    @SerialName("account_id_b64") public val accountIdB64: String,
    /** Echo of the action hash the passkey attested to. */
    @SerialName("action_hash_b64") public val actionHashB64: String,
    @SerialName("credential_id_b64") public val credentialIdB64: String,
    /** URL-safe base64 of the signed `ActionAttestation` (G-13, ADR-0015).
     *  The remove-device flow carries this alongside its MLS Remove commit so
     *  peers can verify it; the delete-passkey flow ignores it. */
    @SerialName("attestation_b64") public val attestationB64: String,
)

/**
 * Phase 2a2c2-android — high-level result of
 * [WebauthnClient.loginWithPasskey]. Mirror of the OPAQUE
 * login result shape, minus the export key (which on Android
 * lives in [me.lakko.app.auth.ExportKeyStore] and is looked
 * up by the [me.lakko.app.auth.AuthRepository] caller after
 * the assertion completes).
 */
public data class PasskeyLoginResult(
    public val accountIdB64: String,
    public val sessionToken: String,
    public val sessionExpiresAtMs: Long,
)

@Serializable
public data class ListCredentialsResponse(
    public val credentials: List<CredentialSummary>,
)

/** A3 — `GET /account/export` shape: what `services/auth` holds about the
 *  account (metadata + auth-factor existence; never the factor secrets). */
@Serializable
public data class AccountExport(
    @SerialName("account_id_b64") public val accountIdB64: String,
    @SerialName("created_at_ms") public val createdAtMs: Long,
    @SerialName("identity_version") public val identityVersion: Long,
    @SerialName("passkey_count") public val passkeyCount: Long,
    @SerialName("has_totp") public val hasTotp: Boolean,
    @SerialName("recovery_contact_kinds") public val recoveryContactKinds: List<Long>,
)

@Serializable
public data class CredentialSummary(
    @SerialName("credential_id_b64") public val credentialIdB64: String,
    public val nickname: String,
    @SerialName("created_at_ms") public val createdAtMs: Long? = null,
    @SerialName("last_used_at_ms") public val lastUsedAtMs: Long? = null,
)

// -----------------------------------------------------------------
// Errors
// -----------------------------------------------------------------

public open class WebauthnClientError(message: String) : Exception(message)

public class WebauthnHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : WebauthnClientError("webauthn $endpoint → $status: $body")

/** The user dismissed the platform sheet, or the authenticator
 *  refused. Mirror of the web `WebauthnUserCancelledError`; the
 *  action-binding gates treat a throw as "do not perform the
 *  action" (fail-closed). */
public class WebauthnUserCancelledError :
    WebauthnClientError("webauthn gesture cancelled")

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) text.take(509) + "…" else text
    } catch (_: Throwable) {
        ""
    }
}

// -----------------------------------------------------------------
// action_hash derivation (G-13, ADR-0014)
// -----------------------------------------------------------------
//
// `action_hash = SHA-256(domain_utf8 || identifier)`, URL-safe
// base64 (no padding, no wrap). SHA-256 is native everywhere
// (WebCrypto / CryptoKit / MessageDigest), so the same convention
// computes identically on every client; the server treats the
// result as opaque 32 bytes. Keep these domain separators in
// lockstep with `apps/web/src/auth/webauthn-client.ts` and the iOS
// client so the stored hash means the same thing on every platform.

/** Domain separator bound into the delete-credential action hash. */
public const val ACTION_DOMAIN_DELETE_CREDENTIAL: String =
    "lakko/action/delete-credential/v1"

/** Domain separator bound into the remove-device action hash. */
public const val ACTION_DOMAIN_REMOVE_DEVICE: String =
    "lakko/action/remove-device/v1"

/** Domain separator bound into the delete-account action hash (A3 — GDPR
 *  erasure). Matches `services/auth`'s `ACTION_DOMAIN_DELETE_ACCOUNT`. */
public const val ACTION_DOMAIN_DELETE_ACCOUNT: String =
    "lakko/action/delete-account/v1"

/** `action_hash = SHA-256(domain_utf8 || identifier)`, URL-safe
 *  base64 with no padding and no wrap. */
private fun deriveActionHash(domain: String, identifier: ByteArray): String {
    val domainBytes = domain.toByteArray(Charsets.UTF_8)
    val input = ByteArray(domainBytes.size + identifier.size)
    System.arraycopy(domainBytes, 0, input, 0, domainBytes.size)
    System.arraycopy(identifier, 0, input, domainBytes.size, identifier.size)
    val digest = MessageDigest.getInstance("SHA-256").digest(input)
    return android.util.Base64.encodeToString(
        digest,
        android.util.Base64.URL_SAFE or android.util.Base64.NO_PADDING or
            android.util.Base64.NO_WRAP,
    )
}

/** Action hash for deleting the passkey credential identified by
 *  [credentialIdB64] (the value [WebauthnClient.listCredentials]
 *  returns). The identifier is the credential id BYTES. */
public fun actionHashForDeleteCredential(credentialIdB64: String): String {
    val idBytes = android.util.Base64.decode(
        credentialIdB64,
        android.util.Base64.URL_SAFE or android.util.Base64.NO_PADDING or
            android.util.Base64.NO_WRAP,
    )
    return deriveActionHash(ACTION_DOMAIN_DELETE_CREDENTIAL, idBytes)
}

/** Action hash for removing the DeviceGroup leaf whose MLS
 *  signature public key is [signaturePublicKey]. */
public fun actionHashForRemoveDevice(signaturePublicKey: ByteArray): String =
    deriveActionHash(ACTION_DOMAIN_REMOVE_DEVICE, signaturePublicKey)

/** Action hash for deleting the account identified by its raw 16-byte
 *  [accountId] (A3). Matches `services/auth`'s `delete_account_action_hash`. */
public fun actionHashForDeleteAccount(accountId: ByteArray): String =
    deriveActionHash(ACTION_DOMAIN_DELETE_ACCOUNT, accountId)
