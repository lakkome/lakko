// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsBytes
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.errorCodeOf
import me.lakko.app.anon.sendWithToken
import me.lakko.app.transport.lakkoHttpEngine

/**
 * ADR-0064 section 1 — the vault routes of `lakko-svc-storage`, under the
 * same base as `/blob` (`<api>/storage`, the anti-censorship engine when
 * armed). No session token is sent on any of them:
 *
 *  * `PUT /vault/{id}` — the claim, one `vault-claim` anonymous token, body
 *    `{"write_pk": "<base64url>"}`: 201 new, 200 same key, 409
 *    `vault-claimed`.
 *  * `PUT /vault/{id}/head` — body `version (u64 BE) || sealed`, accepted
 *    only at the current version + 1 (409 `vault-version-conflict`).
 *  * `GET /vault/{id}/head` — the same shape back; 404 when no head was
 *    written; 401 `vault-proof-refused` for a vault nobody claimed (the
 *    same answer as for a bad proof).
 *  * `DELETE /vault/{id}` — the vault and everything in it; 204.
 *
 * Every request but the claim carries the storage proof over the exact body
 * ([VaultRequestSigner], the core's `signRequest` over the service path).
 */
public class VaultClient(
    private val baseUrl: () -> String = { StorageClient.DEFAULT_BASE_URL },
    engine: HttpClientEngine? = null,
    private val tokens: AnonTokenSource? = if (engine == null) AnonTokens.shared else null,
) : AutoCloseable {
    private val client: HttpClient = HttpClient(engine ?: lakkoHttpEngine())

    override fun close() {
        client.close()
    }

    /** What a head read found. */
    public sealed class HeadRead {
        /** A committed head at [version], its sealed bytes. */
        public class Found(public val version: Long, public val sealed: ByteArray) : HeadRead()

        /** The vault is claimed and holds no head (404). */
        public object Missing : HeadRead()

        /** The proof was refused: the vault was never claimed (or is
         *  claimed under another key, or the clock is off). */
        public object Refused : HeadRead()
    }

    /** What a head write did. */
    public enum class HeadWrite {
        /** Written (201). */
        WRITTEN,

        /** Another write took that version (409): read, merge, write again. */
        CONFLICT,

        /** The proof was refused (401): the vault is not claimed yet. */
        REFUSED,
    }

    /** `GET /vault/{id}/head`. Throws on a network failure or any other
     *  answer. */
    public suspend fun readHead(vaultIdHex: String, signer: VaultRequestSigner): HeadRead {
        val path = headPath(vaultIdHex)
        val response = send(path) {
            client.get("${baseUrl()}$path") { header(signer.headerName, signer.sign("GET", path, ByteArray(0))) }
        }
        return when (val status = response.status.value) {
            200 -> {
                val body = response.bodyAsBytes()
                val parsed = splitVersioned(body) ?: throw VaultHttpError(status, path, "malformed head body")
                HeadRead.Found(parsed.first, parsed.second)
            }
            404 -> HeadRead.Missing
            401 -> HeadRead.Refused
            else -> throw VaultHttpError(status, path, response.safeBody())
        }
    }

    /** `PUT /vault/{id}/head` at [version]. Throws on a network failure or
     *  any answer other than 201 / 409 / 401. */
    public suspend fun writeHead(
        vaultIdHex: String,
        version: Long,
        sealed: ByteArray,
        signer: VaultRequestSigner,
    ): HeadWrite {
        val path = headPath(vaultIdHex)
        val body = joinVersioned(version, sealed)
        val response = send(path) {
            client.put("${baseUrl()}$path") {
                header(signer.headerName, signer.sign("PUT", path, body))
                contentType(ContentType.Application.OctetStream)
                setBody(body)
            }
        }
        return when (val status = response.status.value) {
            200, 201 -> HeadWrite.WRITTEN
            409 -> HeadWrite.CONFLICT
            401 -> HeadWrite.REFUSED
            else -> throw VaultHttpError(status, path, response.safeBody())
        }
    }

    /** `DELETE /vault/{id}`, signed over an empty body. `true` on 204, and
     *  on 401 / 404 (nothing there under this key: never claimed, or gone).
     *  Throws on a network failure or any other answer. */
    public suspend fun deleteVault(vaultIdHex: String, signer: VaultRequestSigner): Boolean {
        val path = "/vault/$vaultIdHex"
        val response = send(path) {
            client.delete("${baseUrl()}$path") { header(signer.headerName, signer.sign("DELETE", path, ByteArray(0))) }
        }
        return when (val status = response.status.value) {
            200, 204 -> true
            401, 404 -> false
            else -> throw VaultHttpError(status, path, response.safeBody())
        }
    }

    /** `PUT /vault/{id}` with one `vault-claim` token. Throws
     *  [VaultClaimedError] when the vault is claimed under another key. */
    public suspend fun claim(vaultIdHex: String, writePublicKey: ByteArray) {
        val path = "/vault/$vaultIdHex"
        val source = tokens ?: throw IllegalStateException("no token source for a vault claim")
        val body = claimBody(writePublicKey)
        val response = send(path) {
            source.sendWithToken(AnonTokenContext.VAULT_CLAIM) { name, value ->
                client.put("${baseUrl()}$path") {
                    header(name, value)
                    contentType(ContentType.Application.Json)
                    setBody(body)
                }
            }
        }
        val status = response.status.value
        if (status == 200 || status == 201) return
        val text = response.safeBody()
        if (status == 409 && errorCodeOf(text) == "vault-claimed") throw VaultClaimedError(path)
        throw VaultHttpError(status, path, text)
    }

    private suspend fun send(path: String, block: suspend () -> HttpResponse): HttpResponse =
        try {
            block()
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: VaultHttpError) {
            throw e
        } catch (cause: Throwable) {
            throw VaultNetworkError(path, cause)
        }

    public companion object {
        /** The service path of a vault's head (what the proof signs). */
        public fun headPath(vaultIdHex: String): String = "/vault/$vaultIdHex/head"

        /** The claim body: `{"write_pk": "<base64url, no padding>"}`. */
        public fun claimBody(writePublicKey: ByteArray): String =
            "{\"write_pk\":\"${java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(writePublicKey)}\"}"

        /** `version (u64 big-endian) || sealed`. */
        public fun joinVersioned(version: Long, sealed: ByteArray): ByteArray {
            val out = ByteArray(8 + sealed.size)
            for (i in 0 until 8) out[i] = (version ushr (56 - 8 * i)).toByte()
            sealed.copyInto(out, 8)
            return out
        }

        /** The version and the sealed bytes of a head body, or `null` when
         *  shorter than the version or the version is zero. */
        public fun splitVersioned(body: ByteArray): Pair<Long, ByteArray>? {
            if (body.size <= 8) return null
            var version = 0L
            for (i in 0 until 8) version = (version shl 8) or (body[i].toLong() and 0xFF)
            if (version <= 0L) return null
            return version to body.copyOfRange(8, body.size)
        }
    }
}

/** The storage proof of one vault request (the core's `signRequest`). */
public interface VaultRequestSigner {
    /** The proof header's name (`x-lakko-storage-proof`). */
    public val headerName: String

    /** The header value for [method] [path] (service path) over [body]. */
    public fun sign(method: String, path: String, body: ByteArray): String
}

private suspend fun HttpResponse.safeBody(): String =
    try {
        bodyAsText().take(512)
    } catch (_: Throwable) {
        ""
    }

/** The storage service answered a vault request with an unexpected status. */
public class VaultHttpError(public val status: Int, public val endpoint: String, public val body: String) :
    RuntimeException("vault $endpoint -> $status")

/** A vault request did not reach the service. */
public class VaultNetworkError(public val endpoint: String, cause: Throwable) :
    RuntimeException("vault $endpoint network error", cause)

/** The vault is claimed under another write key. */
public class VaultClaimedError(public val endpoint: String) : RuntimeException("vault $endpoint is claimed under another key")
