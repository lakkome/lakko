// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
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
 *     sign-in pair, driven by [loginWithPasskey].
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
        HttpClient(lakkoHttpEngine()) { install(ContentNegotiation) { json(json) } }
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
        freshGrant: String,
    ): RegisterStartResponse {
        val req = RegisterStartRequest(userName = userName, nickname = nickname, freshGrant = freshGrant)
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
        freshGrant: String,
    ): RegisterFinishResponse {
        val req = RegisterFinishRequest(
            stateId = stateId,
            nickname = nickname,
            credential = credential,
            freshGrant = freshGrant,
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
        deviceIdB64Url: String? = null,
    ): AuthFinishResponse {
        val req = AuthFinishRequest(stateId = stateId, credential = credential, deviceIdB64 = deviceIdB64Url)
        val response = client.post("$baseUrl/webauthn/auth/finish") {
            contentType(ContentType.Application.Json)
            setBody(req)
        }
        ensure2xx(response, "/webauthn/auth/finish")
        return response.body()
    }

    /**
     * Drive the full passkey sign-in round. Mirror of
     * `WebauthnClient.loginWithPasskey` on web + iOS.
     *
     * Discoverable by default (the start body is `{}`): no account has to be
     * cached on this device and no name typed; the Credential Manager offers
     * any passkey it holds for the Lakko relying-party id. With
     * [accountIdB64] the start is limited to that account's passkeys.
     *
     * The assertion also asks for the PRF evaluation ([PASSKEY_PRF_SALT]),
     * so a passkey that carries the account's keys can restore them; the
     * output is in [PasskeyLoginResult.prfOutput] and is never sent anywhere.
     *
     * Returns `null` when the user cancelled the sheet. Throws
     * on any other failure mode ([WebauthnHttpError], etc.).
     */
    public suspend fun loginWithPasskey(
        context: android.content.Context,
        accountIdB64: String? = null,
        registration: WebauthnRegistration = WebauthnRegistration(),
        /** ADR-0074 - base64url of this device's id, when the account is
         *  known beforehand ([accountIdB64] given); else the session lands
         *  naming no device and is bound afterwards. */
        deviceIdB64Url: String? = null,
    ): PasskeyLoginResult? {
        val start = startAuthentication(accountIdB64 = accountIdB64)
        val publicKey = start.options.publicKeyField()
        return when (
            val outcome = registration.authenticate(context, publicKey, prfSalt = PASSKEY_PRF_SALT)
        ) {
            is WebauthnRegistration.Outcome.Success -> {
                val finish = finishAuthentication(
                    stateId = start.stateId,
                    credential = outcome.value.credential,
                    deviceIdB64Url = deviceIdB64Url,
                )
                PasskeyLoginResult(
                    accountIdB64 = finish.accountIdB64,
                    session = me.lakko.app.auth.SessionPair(
                        accessToken = finish.sessionToken,
                        accessExpiresAtMs = finish.sessionExpiresAtMs,
                        refreshToken = finish.refreshToken,
                        refreshExpiresAtMs = finish.refreshExpiresAtMs,
                        keyGeneration = finish.keyGeneration,
                    ),
                    credentialId = outcome.value.credentialId,
                    prfOutput = outcome.value.prfOutput,
                    freshGrant = finish.freshGrant,
                    freshGrantExpiresAtMs = finish.freshGrantExpiresAtMs,
                )
            }
            WebauthnRegistration.Outcome.Cancelled -> null
            is WebauthnRegistration.Outcome.Failed -> throw outcome.cause
        }
    }

    /**
     * Create a passkey for the signed-in account: `POST /register/start`,
     * the Credential Manager sheet (asking the new credential for the PRF
     * evaluation), `POST /register/finish`. Nothing is stored for the
     * passkey beyond the credential itself; whether it can also restore keys
     * is the person's later choice.
     *
     * Both calls carry [freshGrant], the fresh-auth proof (UX section 17):
     * a session alone may not add a passkey, since a passkey is itself a
     * proof. Without a usable one the server answers `403
     * fresh-auth-required`.
     *
     * @throws WebauthnUserCancelledError when the sheet was dismissed.
     */
    public suspend fun registerPasskey(
        context: android.content.Context,
        userName: String,
        nickname: String,
        freshGrant: String,
        registration: WebauthnRegistration = WebauthnRegistration(),
    ): PasskeyAdded {
        val start = startRegistration(userName, nickname, freshGrant)
        val publicKey = start.options.publicKeyField()
        return when (
            val outcome = registration.register(context, publicKey, prfSalt = PASSKEY_PRF_SALT)
        ) {
            is WebauthnRegistration.Outcome.Success -> {
                val finish = finishRegistration(
                    stateId = start.stateId,
                    nickname = start.nickname,
                    credential = outcome.value.credential,
                    freshGrant = freshGrant,
                )
                PasskeyAdded(
                    credentialIdB64 = finish.credentialIdB64,
                    prfOutput = outcome.value.prfOutput,
                    prfEnabled = outcome.value.prfEnabled,
                )
            }
            WebauthnRegistration.Outcome.Cancelled -> throw WebauthnUserCancelledError()
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
    ): ActionFinishResponse =
        assertActionCeremony(context, actionHashB64, registration, prfSalt = null, onlyCredentialIdB64 = null).first

    private suspend fun assertActionCeremony(
        context: android.content.Context,
        actionHashB64: String,
        registration: WebauthnRegistration,
        prfSalt: ByteArray?,
        onlyCredentialIdB64: String?,
    ): Pair<ActionFinishResponse, WebauthnRegistration.Ceremony> {
        val start = startAction(actionHashB64)
        val publicKey = start.options.publicKeyField()
        return when (
            val outcome = registration.authenticate(
                context,
                publicKey,
                prfSalt = prfSalt,
                onlyCredentialIdB64 = onlyCredentialIdB64,
            )
        ) {
            is WebauthnRegistration.Outcome.Success -> finishAction(
                stateId = start.stateId,
                actionHashB64 = actionHashB64,
                credential = outcome.value.credential,
            ) to outcome.value
            WebauthnRegistration.Outcome.Cancelled ->
                throw WebauthnUserCancelledError()
            is WebauthnRegistration.Outcome.Failed -> throw outcome.cause
        }
    }

    /**
     * The PRF output of ONE of the signed-in account's passkeys, obtained
     * inside the session (no new sign-in): the action ceremony bound to
     * "this passkey restores keys", limited to [credentialIdB64], with the
     * PRF evaluation requested. `prfOutput` is `null` when the provider
     * returned none. The caller checks [PasskeyCeremonyExtras.credentialId]
     * against the passkey it meant.
     *
     * @throws WebauthnUserCancelledError when the sheet was dismissed.
     */
    public suspend fun evaluatePrfFor(
        context: android.content.Context,
        credentialIdB64: String,
        registration: WebauthnRegistration = WebauthnRegistration(),
    ): PasskeyCeremonyExtras {
        val (finish, ceremony) = assertActionCeremony(
            context = context,
            actionHashB64 = actionHashForPasskeyRestoresKeys(credentialIdB64),
            registration = registration,
            prfSalt = PASSKEY_PRF_SALT,
            onlyCredentialIdB64 = credentialIdB64,
        )
        return PasskeyCeremonyExtras(
            // The server's verified answer names the credential that signed.
            credentialIdB64 = finish.credentialIdB64,
            prfOutput = ceremony.prfOutput,
        )
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
     *  a [WebauthnHttpError].
     *
     *  UX 17.3: the route also needs the fresh-auth proof, carried in the
     *  `x-lakko-fresh-grant` header (it has no body). Without a usable one
     *  the server answers `403 {"error":"fresh-auth-required"}`
     *  ([me.lakko.app.auth.isFreshAuthRequired]), which is a different 403
     *  from the passkey action grant's. */
    public suspend fun deleteAccount(freshGrant: String?) {
        val endpoint = "/account"
        val response = client.delete("$baseUrl$endpoint") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            freshGrant?.takeIf { it.isNotEmpty() }?.let { header(me.lakko.app.auth.FRESH_GRANT_HEADER, it) }
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
        /** The auth service base, same as [me.lakko.app.auth.AuthClient.DEFAULT_BASE_URL]:
         *  `<LAKKO_API_BASE_URL>/auth`, or `http://10.0.2.2:8081` without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.AUTH)
    }
}

// -----------------------------------------------------------------
// Wire shapes
// -----------------------------------------------------------------

@Serializable
public data class RegisterStartRequest(
    @SerialName("user_name") public val userName: String,
    public val nickname: String,
    /** The fresh-auth proof (UX section 17). */
    @SerialName("fresh_grant") public val freshGrant: String,
) {
    override fun toString(): String = "RegisterStartRequest(<redacted>)"
}

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
    /** The same fresh-auth proof `start` carried. */
    @SerialName("fresh_grant") public val freshGrant: String,
) {
    override fun toString(): String = "RegisterFinishRequest(<redacted>)"
}

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
    /** ADR-0074 - absent = the session names no device yet. */
    @SerialName("device_id_b64") public val deviceIdB64: String? = null,
)

@Serializable
public data class AuthFinishResponse(
    @SerialName("account_id_b64") public val accountIdB64: String,
    @SerialName("session_token") public val sessionToken: String,
    @SerialName("session_expires_at_ms") public val sessionExpiresAtMs: Long,
    /** ADR-0073: the rest of the session pair. */
    @SerialName("refresh_token") public val refreshToken: String,
    @SerialName("refresh_expires_at_ms") public val refreshExpiresAtMs: Long,
    @SerialName("key_generation") public val keyGeneration: Long,
    /** The fresh-auth proof the passkey sign-in minted (UX section 17);
     *  opaque, never stored, never logged. */
    @SerialName("fresh_grant") public val freshGrant: String? = null,
    @SerialName("fresh_grant_expires_at_ms") public val freshGrantExpiresAtMs: Long? = null,
) {
    override fun toString(): String = "AuthFinishResponse(<redacted>)"
}

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
 * The account id as `POST /webauthn/auth/start` takes it: URL-safe base64
 * without padding (`services/auth` decodes it with `URL_SAFE_NO_PAD`; a
 * padded or standard-alphabet value is a `400`). Elsewhere on Android an
 * account id is held as standard base64 with padding ([me.lakko.app.auth.base64Encode]),
 * so the passkey path converts at this boundary, from the bytes.
 */
public fun passkeyStartAccountId(accountIdBytes: ByteArray): String =
    me.lakko.app.auth.base64UrlEncodeNoPad(accountIdBytes)

/**
 * The 16 account-id bytes of a `POST /webauthn/auth/finish` answer, whose
 * `account_id_b64` is URL-safe base64 without padding; `null` when it is
 * not that or not 16 bytes. Callers compare the bytes, never the text.
 */
public fun passkeyFinishAccountId(accountIdB64: String): ByteArray? {
    val bytes = try {
        me.lakko.app.auth.base64UrlDecodeNoPad(accountIdB64)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return bytes.takeIf { it.size == PASSKEY_ACCOUNT_ID_LENGTH }
}

private const val PASSKEY_ACCOUNT_ID_LENGTH = 16

/**
 * The result of [WebauthnClient.loginWithPasskey]: the session triple the
 * server issued, plus what stays on this device: the id of the passkey that
 * answered and its PRF output when the provider returned one.
 */
public class PasskeyLoginResult(
    /** base64url, no padding (the server's wire form). */
    public val accountIdB64: String,
    /** ADR-0073: the session pair the assertion landed. */
    public val session: me.lakko.app.auth.SessionPair,
    public val credentialId: ByteArray?,
    /** 32 bytes, or `null`: the passkey then signs in only. Never sent to
     *  the server. */
    public val prfOutput: ByteArray?,
    /** The fresh-auth proof of this sign-in, opaque; memory only. */
    public val freshGrant: String? = null,
    public val freshGrantExpiresAtMs: Long? = null,
) {
    /** The same result with [pair] as its session (ADR-0074: the pair
     *  `bind-device` returned for a sign-in that named no device). */
    public fun withSession(pair: me.lakko.app.auth.SessionPair): PasskeyLoginResult = PasskeyLoginResult(
        accountIdB64 = accountIdB64,
        session = pair,
        credentialId = credentialId,
        prfOutput = prfOutput,
        freshGrant = freshGrant,
        freshGrantExpiresAtMs = freshGrantExpiresAtMs,
    )

    public val sessionToken: String get() = session.accessToken

    public val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs
}

/** The result of [WebauthnClient.registerPasskey]. */
public class PasskeyAdded(
    /** base64url, no padding. */
    public val credentialIdB64: String,
    public val prfOutput: ByteArray?,
    public val prfEnabled: Boolean?,
)

/** What [WebauthnClient.evaluatePrfFor] yields. */
public class PasskeyCeremonyExtras(
    /** The passkey that answered (base64url, no padding). */
    public val credentialIdB64: String,
    public val prfOutput: ByteArray?,
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

/** The passkey provider refused to create a passkey because it already
 *  holds one for this account (WebAuthn `InvalidStateError`). Nothing was
 *  created; the line is `settings.passkeys.error.already_exists`. */
public class PasskeyAlreadyExistsError(cause: Throwable? = null) :
    WebauthnClientError("the passkey provider already holds a passkey for this account") {
    init {
        if (cause != null) initCause(cause)
    }
}

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

/** Domain separator bound into the action hash of the ceremony that reads a
 *  passkey's PRF output inside a session ("this passkey restores keys").
 *  Same string as the web client. */
public const val ACTION_DOMAIN_PASSKEY_RESTORES_KEYS: String =
    "lakko/action/passkey-restores-keys/v1"

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

/** Action hash for reading the PRF output of the passkey [credentialIdB64]
 *  inside a session. The identifier is the credential id BYTES. */
public fun actionHashForPasskeyRestoresKeys(credentialIdB64: String): String {
    val idBytes = android.util.Base64.decode(
        credentialIdB64,
        android.util.Base64.URL_SAFE or android.util.Base64.NO_PADDING or
            android.util.Base64.NO_WRAP,
    )
    return deriveActionHash(ACTION_DOMAIN_PASSKEY_RESTORES_KEYS, idBytes)
}
