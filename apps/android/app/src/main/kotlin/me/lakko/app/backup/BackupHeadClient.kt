// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.transport.AntiCensorshipEngine

/**
 * Backup-head mint client — `POST /auth/backup/head` (ADR-0037 §2, G-11).
 *
 * Mirror of `apps/web/src/backup/head-mint.ts`.
 *
 * The auth service holds the per-account monotone `backup_version` counter and
 * signs a 129-byte `BackupHead` committing the envelope the client is about to
 * store. This is the ONE round-trip the upload path adds: after building the
 * AEAD envelope the client computes its `BLAKE3` (the FFI `backupEnvelopeHash`
 * helper), mints the head here, and frames `TAG ‖ head ‖ envelope` into the
 * single storage `PUT`.
 *
 * Bearer-authenticated: holding a valid session token IS proof the caller
 * OPAQUE-authenticated as `account_id`, which is the security basis — auth binds
 * THAT account to the client-presented envelope hash. No PoW (mirrors the
 * genesis-attest mint).
 *
 * A mint failure is **soft** on the upload side (ADR-0037 §6): the caller
 * ([BackupClient.uploadNow]) falls back to writing the unframed legacy envelope
 * rather than failing the whole backup. So [mint] simply throws on any failure
 * and lets the seam decide.
 *
 * The endpoint lives on the **auth** service (default
 * [AuthClient.DEFAULT_BASE_URL]), not storage; the request rides the
 * anti-censorship tunnel when armed (ADR-0026), matching every other auth call.
 */
public class BackupHeadClient(
    private val baseUrl: String = AuthClient.DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` via [AntiCensorshipEngine] (default
     *  below). Tests: `MockEngine` for deterministic responses. */
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val json = Json { ignoreUnknownKeys = true }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) { install(ContentNegotiation) { json(json) } }
    } else {
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    /**
     * Mint an auth-signed backup head for `envelopeHash`.
     *
     * @param sessionToken bearer token proving the OPAQUE auth (`null` / empty →
     *                     throws, so the soft-fail path writes the legacy
     *                     envelope).
     * @param envelopeHash the 32-byte `BLAKE3` of the envelope about to be
     *                     stored (from the FFI `backupEnvelopeHash`).
     * @throws IllegalStateException on a null/empty token or malformed response.
     * @throws RuntimeException on a non-2xx or transport failure.
     */
    public suspend fun mint(
        sessionToken: String?,
        envelopeHash: ByteArray,
    ): MintedBackupHead {
        if (sessionToken.isNullOrEmpty()) {
            throw IllegalStateException("mintBackupHead: no session token")
        }
        val response: HttpResponse = client.post("$baseUrl/auth/backup/head") {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
            contentType(ContentType.Application.Json)
            setBody(
                BackupHeadRequest(
                    envelope_hash_b64 = base64UrlEncodeNoPad(envelopeHash),
                ),
            )
        }
        if (!response.status.isSuccess()) {
            throw RuntimeException(
                "mintBackupHead: /auth/backup/head -> ${response.status.value}",
            )
        }
        val wire: BackupHeadResponse = response.body()
        val head = base64UrlDecodeNoPad(wire.backup_head_b64)
        return MintedBackupHead(head = head, version = wire.backup_version)
    }
}

/** The minted head + its auth-assigned monotone version (ADR-0037 §2). */
public data class MintedBackupHead(
    /** The raw signed `BackupHead` bytes (decoded from `backup_head_b64`). */
    val head: ByteArray,
    /** The auth-assigned monotone `backup_version` — the floor a future restore
     *  on this device lineage compares against. `ULong` to match the FFI
     *  `VerifiedBackupHead.backupVersion` and never lose precision on a u64. */
    val version: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is MintedBackupHead) return false
        return version == other.version && head.contentEquals(other.head)
    }

    override fun hashCode(): Int = 31 * version.hashCode() + head.contentHashCode()
}

/** `POST /auth/backup/head` request body. Bearer-authenticated (the session
 *  token rides the `Authorization` header, not the body). */
@Serializable
private data class BackupHeadRequest(
    /** URL-safe base64 (no pad) of the 32-byte `BLAKE3` envelope hash. */
    val envelope_hash_b64: String,
)

/** `POST /auth/backup/head` response body. */
@Serializable
private data class BackupHeadResponse(
    /** URL-safe base64 (no pad) of the signed `BackupHead` bytes. */
    val backup_head_b64: String,
    /** The auth-assigned monotone `backup_version` (u64). */
    val backup_version: ULong,
)
