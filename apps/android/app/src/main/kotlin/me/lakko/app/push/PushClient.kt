// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.PowProofPayload
import me.lakko.app.auth.base64Encode
import me.lakko.app.auth.solvePow

/**
 * Ktor-based client for `lakko-svc-push-gw`.
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/client.ts` (1.20b)
 *   * `apps/ios/Lakko/Push/PushClient.swift` (1.20c)
 *
 * Two endpoints from the user-device side:
 *
 *   * `POST /push/register` — bind a
 *     `(device_pseudonym, provider)` pair to an FCM token.
 *     Idempotent: the server UPSERTs the row.
 *   * `POST /push/unregister` — drop a token. Called on
 *     logout + forgetDevice.
 *
 * Plus `/pow/challenge` for the per-call PoW seed.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8085` — Android-emulator loopback alias,
 * port `8085` matching `lakko-svc-push-gw`'s dev bind. Override
 * via the constructor for physical devices (LAN IP) and
 * production (Cloudflare tunnel URL).
 */
public class PushClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a
     *  network. */
    engine: HttpClientEngine? = null,
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
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    /** Provider names — must match the snake_case `serde`
     *  rename on `ProviderKind` in
     *  `services/push-gw/src/provider.rs`. */
    @Serializable
    public enum class Provider {
        @SerialName("apns")
        APNS,
        @SerialName("fcm")
        FCM,
        @SerialName("web_push")
        WEB_PUSH,
        @SerialName("unified_push")
        UNIFIED_PUSH,
    }

    /**
     * Bind `(devicePseudonym, provider) → tokenBytes`.
     * Idempotent.
     *
     * @param devicePseudonym 32 raw bytes (mirror of the
     *                        `DEVICE_LEN` constant in
     *                        `services/push-gw/src/registry.rs`).
     * @param provider        Which delivery path this token is
     *                        for.
     * @param tokenBytes      Provider-opaque bytes. For FCM
     *                        this is the UTF-8 encoded
     *                        registration token string the
     *                        Firebase SDK handed us.
     */
    public suspend fun register(
        devicePseudonym: ByteArray,
        provider: Provider,
        tokenBytes: ByteArray,
    ) {
        require(devicePseudonym.size == PSEUDONYM_LEN) {
            "devicePseudonym must be $PSEUDONYM_LEN bytes"
        }
        require(tokenBytes.isNotEmpty()) { "tokenBytes must be non-empty" }

        val challenge = fetchChallenge()
        val pow = solvePow(seedB64 = challenge.seed, effortBits = challenge.effortBits)

        val req = RegisterRequest(
            pow = pow,
            device_pseudonym = pseudonymHex(devicePseudonym),
            provider = provider,
            token = base64Encode(tokenBytes),
        )
        val response: HttpResponse = try {
            client.post("$baseUrl/push/register") {
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/register", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/register",
                body = response.safeReadBody(),
            )
        }
        val resp: RegisterResponse = response.body()
        if (!resp.ok) {
            throw PushHttpError(
                status = 200,
                endpoint = "/push/register",
                body = "server reported ok=false",
            )
        }
    }

    // -----------------------------------------------------------------
    // Group fan-out (Phase 1.20e)
    // -----------------------------------------------------------------

    /**
     * Subscribe this device to push wakes for `groupPseudonym`.
     * Idempotent — re-subscribing the same `(device, group)`
     * pair returns `false` (no insert) and is a no-op.
     */
    public suspend fun subscribeGroup(
        devicePseudonym: ByteArray,
        groupPseudonym: ByteArray,
    ): Boolean {
        require(devicePseudonym.size == PSEUDONYM_LEN) {
            "devicePseudonym must be $PSEUDONYM_LEN bytes"
        }
        require(groupPseudonym.size == PSEUDONYM_LEN) {
            "groupPseudonym must be $PSEUDONYM_LEN bytes"
        }
        val challenge = fetchChallenge()
        val pow = solvePow(seedB64 = challenge.seed, effortBits = challenge.effortBits)
        val req = GroupSubscribeRequest(
            pow = pow,
            device_pseudonym = pseudonymHex(devicePseudonym),
            group_pseudonym = pseudonymHex(groupPseudonym),
        )
        val response: HttpResponse = try {
            client.post("$baseUrl/push/group/subscribe") {
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/group/subscribe", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/group/subscribe",
                body = response.safeReadBody(),
            )
        }
        return response.body<GroupSubscribeResponse>().inserted
    }

    /** Drop a `(device, group)` subscription. Idempotent. */
    public suspend fun unsubscribeGroup(
        devicePseudonym: ByteArray,
        groupPseudonym: ByteArray,
    ): Boolean {
        require(devicePseudonym.size == PSEUDONYM_LEN)
        require(groupPseudonym.size == PSEUDONYM_LEN)
        val challenge = fetchChallenge()
        val pow = solvePow(seedB64 = challenge.seed, effortBits = challenge.effortBits)
        val req = GroupUnsubscribeRequest(
            pow = pow,
            device_pseudonym = pseudonymHex(devicePseudonym),
            group_pseudonym = pseudonymHex(groupPseudonym),
        )
        val response: HttpResponse = try {
            client.post("$baseUrl/push/group/unsubscribe") {
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/group/unsubscribe", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/group/unsubscribe",
                body = response.safeReadBody(),
            )
        }
        return response.body<GroupUnsubscribeResponse>().removed
    }

    /**
     * Fan a wake out to every device subscribed to
     * `groupPseudonym`. Phase 1.20e: the publisher's client
     * calls this after a successful `DsClient.publishGroup`.
     */
    public suspend fun notifyGroup(
        groupPseudonym: ByteArray,
        envelopePointer: Long? = null,
    ): Int {
        require(groupPseudonym.size == PSEUDONYM_LEN)
        val challenge = fetchChallenge()
        val pow = solvePow(seedB64 = challenge.seed, effortBits = challenge.effortBits)
        val req = NotifyGroupRequest(
            pow = pow,
            group_pseudonym = pseudonymHex(groupPseudonym),
            envelope_pointer = envelopePointer?.toULong(),
        )
        val response: HttpResponse = try {
            client.post("$baseUrl/push/notify-group") {
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/notify-group", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/notify-group",
                body = response.safeReadBody(),
            )
        }
        return response.body<NotifyGroupResponse>().devices_woken
    }

    /**
     * Drop a previously-registered token. Idempotent: returns
     * `false` if the row was already gone.
     */
    public suspend fun unregister(
        devicePseudonym: ByteArray,
        provider: Provider,
    ): Boolean {
        require(devicePseudonym.size == PSEUDONYM_LEN) {
            "devicePseudonym must be $PSEUDONYM_LEN bytes"
        }
        val challenge = fetchChallenge()
        val pow = solvePow(seedB64 = challenge.seed, effortBits = challenge.effortBits)

        val req = UnregisterRequest(
            pow = pow,
            device_pseudonym = pseudonymHex(devicePseudonym),
            provider = provider,
        )
        val response: HttpResponse = try {
            client.post("$baseUrl/push/unregister") {
                contentType(ContentType.Application.Json)
                setBody(req)
            }
        } catch (cause: Throwable) {
            throw PushNetworkError("/push/unregister", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/push/unregister",
                body = response.safeReadBody(),
            )
        }
        val resp: UnregisterResponse = response.body()
        if (!resp.ok) {
            throw PushHttpError(
                status = 200,
                endpoint = "/push/unregister",
                body = "server reported ok=false",
            )
        }
        return resp.removed
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private suspend fun fetchChallenge(): PowChallenge {
        val response: HttpResponse = try {
            client.post("$baseUrl/pow/challenge")
        } catch (cause: Throwable) {
            throw PushNetworkError("/pow/challenge", cause)
        }
        if (!response.status.isSuccess()) {
            throw PushHttpError(
                status = response.status.value,
                endpoint = "/pow/challenge",
                body = response.safeReadBody(),
            )
        }
        return response.body()
    }

    public companion object {
        /** Default URL — `<LAKKO_API_BASE_URL>/push-gw` ([ApiEndpoints]),
         *  or the Android emulator loopback alias + the dev
         *  `lakko-svc-push-gw` bind (`http://10.0.2.2:8085`) without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.PUSH_GW)

        /** Raw device pseudonym length — must match
         *  `DEVICE_LEN` in
         *  `services/push-gw/src/registry.rs`. */
        public const val PSEUDONYM_LEN: Int = 32

        /** Lowercase-hex encode the device pseudonym. */
        public fun pseudonymHex(bytes: ByteArray): String {
            val sb = StringBuilder(bytes.size * 2)
            for (b in bytes) {
                val v = b.toInt() and 0xFF
                sb.append(HEX[v ushr 4])
                sb.append(HEX[v and 0x0F])
            }
            return sb.toString()
        }

        private const val HEX: String = "0123456789abcdef"
    }
}

// =============================================================================
// Wire shapes
// =============================================================================

@Serializable
@Suppress("ConstructorParameterNaming")
private data class PowChallenge(
    val seed: String,
    @SerialName("effort_bits") val effortBits: Int,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class RegisterRequest(
    val pow: PowProofPayload,
    val device_pseudonym: String,
    val provider: PushClient.Provider,
    val token: String,
)

@Serializable
private data class RegisterResponse(
    val ok: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class UnregisterRequest(
    val pow: PowProofPayload,
    val device_pseudonym: String,
    val provider: PushClient.Provider,
)

@Serializable
private data class UnregisterResponse(
    val ok: Boolean,
    val removed: Boolean,
)

// 1.20e — group fan-out wire shapes.

@Serializable
@Suppress("ConstructorParameterNaming")
private data class GroupSubscribeRequest(
    val pow: PowProofPayload,
    val device_pseudonym: String,
    val group_pseudonym: String,
)

@Serializable
private data class GroupSubscribeResponse(
    val ok: Boolean,
    val inserted: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class GroupUnsubscribeRequest(
    val pow: PowProofPayload,
    val device_pseudonym: String,
    val group_pseudonym: String,
)

@Serializable
private data class GroupUnsubscribeResponse(
    val ok: Boolean,
    val removed: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class NotifyGroupRequest(
    val pow: PowProofPayload,
    val group_pseudonym: String,
    val envelope_pointer: ULong?,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class NotifyGroupResponse(
    val devices_woken: Int,
)

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}

// =============================================================================
// Errors
// =============================================================================

/** Server returned a non-2xx. */
public class PushHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : RuntimeException("push $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). */
public class PushNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("push $endpoint network error: $cause", cause)
