// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import io.ktor.client.HttpClient
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.request.get
import io.ktor.client.statement.readBytes
import io.ktor.http.isSuccess
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.webauthn.WebauthnClient

/**
 * Device-attestation verifying key (G-13, ADR-0015 sprint 5).
 *
 * Peer devices verifying a removal attestation need the 32-byte Ed25519
 * public key the auth service signs attestations with, published as an
 * OKP / Ed25519 JWK at `GET /.well-known/device-attest-jwks.json`. Mirror of
 * `apps/web/src/auth/device-attest-key.ts`.
 *
 * In the zero-trust posture this key should ideally be PINNED via the signed
 * bootstrap config rather than fetched from the protected origin; this is the
 * discovery path for the current single-origin deployment. The fetched key is
 * only ever handed to `DeviceGroup.processIncomingVerifyingRemovals`, which
 * performs the Ed25519 verification in the audited Rust core.
 */
object DeviceAttestKey {
    @Serializable
    private data class Jwk(@SerialName("x") val x: String? = null)

    @Serializable
    private data class Jwks(@SerialName("keys") val keys: List<Jwk>? = null)

    private val json = Json { ignoreUnknownKeys = true }
    private val client = HttpClient(OkHttp)
    private const val TTL_MS = 24L * 60 * 60 * 1000

    @Volatile private var cachedKey: ByteArray? = null

    @Volatile private var cachedAtMs: Long = 0

    /** Fetch (and cache for 24 h) the 32-byte device-attestation verifying key. */
    suspend fun fetch(
        baseUrl: String = WebauthnClient.DEFAULT_BASE_URL,
        nowMs: Long = System.currentTimeMillis(),
    ): ByteArray {
        cachedKey?.let { if (nowMs - cachedAtMs < TTL_MS) return it }
        val response = client.get("$baseUrl/.well-known/device-attest-jwks.json")
        require(response.status.isSuccess()) {
            "device-attest JWKS fetch failed: ${response.status.value}"
        }
        val text = String(response.readBytes(), Charsets.UTF_8)
        val jwks = json.decodeFromString(Jwks.serializer(), text)
        val x = jwks.keys?.firstOrNull()?.x
            ?: throw IllegalStateException("device-attest JWKS has no key")
        val key = base64UrlDecodeNoPad(x)
        require(key.size == 32) { "device-attest key is ${key.size} bytes, expected 32" }
        cachedKey = key
        cachedAtMs = nowMs
        return key
    }
}
