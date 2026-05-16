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

/**
 * Ktor-based client for `lakko-svc-auth`'s `/webauthn/*`
 * endpoints (Phase 2a2e).
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
     *  (`/register/*`, `/credentials`, `/credentials/:id`). The
     *  anonymous `/auth/*` routes ignore it. */
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

    public suspend fun deleteCredential(credentialIdB64: String) {
        val endpoint = "/webauthn/credentials/$credentialIdB64"
        val response = client.delete("$baseUrl$endpoint") {
            sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
        }
        // 404 is idempotent — already deleted.
        if (response.status == HttpStatusCode.NotFound) return
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

@Serializable
public data class ListCredentialsResponse(
    public val credentials: List<CredentialSummary>,
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

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) text.take(509) + "…" else text
    } catch (_: Throwable) {
        ""
    }
}
