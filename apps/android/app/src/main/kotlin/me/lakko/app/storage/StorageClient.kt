// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.request.get
import io.ktor.client.request.head
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsBytes
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType

/**
 * Ktor-based client for `lakko-svc-storage`.
 *
 * Mirror of:
 *
 *   * `apps/web/src/storage/client.ts` (1.19c1)
 *   * `apps/ios/Lakko/Storage/StorageClient.swift` (1.19c1)
 *
 * Content-addressable blob proxy with three verbs:
 *
 *   * `PUT  /blob/<sha256_hex>` — body = ciphertext. Idempotent:
 *     the server verifies `SHA-256(body) == <sha256_hex>` and
 *     stores the bytes at that key. 201 on first upload, 200 on
 *     re-upload, 400 on hash mismatch.
 *   * `GET  /blob/<sha256_hex>` — body = ciphertext. 200 on hit,
 *     404 on miss.
 *   * `HEAD /blob/<sha256_hex>` — same shape, headers only. The
 *     `Content-Length` header sizes the download.
 *
 * ## Auth posture
 *
 * Phase-1 storage is unauthenticated — a "public CDN for E2EE
 * blobs". The server-side hash verification on PUT prevents
 * poisoning existing blobs (SHA-256 collisions are infeasible),
 * and nginx-layer rate limits curb churn. Owner tracking + quota
 * land in Phase 2; the client signature stays the same.
 *
 * ## Hash boundary
 *
 * The `blobId` parameter is **always** the raw 32-byte SHA-256
 * of the *ciphertext* — exactly what `EncryptedAttachment.blobId`
 * returns from the UniFFI surface (`lakko-attachments` 1.19a +
 * FFI exposure 1.19b). Callers do not compute the hash
 * themselves; they hand the bytes from the encryption layer
 * straight through.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8083` — Android-emulator loopback alias, port
 * `8083` matching `lakko-svc-storage`'s dev bind. Override via
 * the constructor for physical devices (LAN IP) and production
 * (Cloudflare tunnel URL).
 */
public class StorageClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a
     *  network. */
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine)
    } else {
        HttpClient(OkHttp)
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // PUT
    // -----------------------------------------------------------------

    /**
     * `PUT /blob/<hex>` — upload ciphertext. Idempotent: re-PUT
     * of the same hash overwrites with byte-identical content
     * (which is a no-op by the meaning of content addressing).
     *
     * The server hashes the body and compares against the URL
     * slug; a mismatch surfaces as [StorageHttpError] with
     * `status = 400`. This lets the caller detect a buggy local
     * encryption path before any recipient sees a broken blob.
     *
     * @param blobId      32-byte SHA-256 of the ciphertext (the
     *                    `blob_id` returned by
     *                    `EncryptedAttachment.blobId`).
     * @param ciphertext  The bytes to store. Must hash to
     *                    `blobId`.
     */
    public suspend fun putBlob(blobId: ByteArray, ciphertext: ByteArray) {
        if (blobId.size != BLOB_ID_LEN) {
            throw StorageClientArgError(
                "blobId must be $BLOB_ID_LEN bytes (got ${blobId.size})",
            )
        }
        if (ciphertext.isEmpty()) {
            throw StorageClientArgError("ciphertext must be non-empty")
        }
        if (ciphertext.size > MAX_BLOB_BYTES) {
            throw StorageClientArgError(
                "ciphertext exceeds the $MAX_BLOB_BYTES-byte ceiling " +
                    "(got ${ciphertext.size} bytes)",
            )
        }

        val hex = blobIdHex(blobId)
        val endpoint = "/blob/$hex"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.put(url) {
                contentType(ContentType.Application.OctetStream)
                setBody(ciphertext)
            }
        } catch (cause: Throwable) {
            throw StorageNetworkError(endpoint, cause)
        }
        // 200 OK = idempotent re-upload, 201 Created = first
        // upload. Both are success.
        val status = response.status.value
        if (status == 200 || status == 201) {
            return
        }
        throw StorageHttpError(
            status = status,
            endpoint = endpoint,
            body = response.safeReadBody(),
        )
    }

    // -----------------------------------------------------------------
    // GET
    // -----------------------------------------------------------------

    /**
     * `GET /blob/<hex>` — fetch ciphertext. Returns the raw
     * bytes on 200, `null` on 404 (typed not-found path so the
     * UI can surface "attachment missing" without an exception).
     * Throws on any other non-2xx.
     */
    public suspend fun getBlob(blobId: ByteArray): ByteArray? {
        if (blobId.size != BLOB_ID_LEN) {
            throw StorageClientArgError(
                "blobId must be $BLOB_ID_LEN bytes (got ${blobId.size})",
            )
        }
        val hex = blobIdHex(blobId)
        val endpoint = "/blob/$hex"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.get(url)
        } catch (cause: Throwable) {
            throw StorageNetworkError(endpoint, cause)
        }
        val status = response.status.value
        if (status == 404) return null
        if (status !in 200..299) {
            throw StorageHttpError(
                status = status,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        return response.bodyAsBytes()
    }

    // -----------------------------------------------------------------
    // HEAD
    // -----------------------------------------------------------------

    /**
     * `HEAD /blob/<hex>` — return the blob's `Content-Length` if
     * it exists, `null` if not. Used by the UI to size a
     * download indicator before issuing the GET.
     *
     * Throws on any non-200 / non-404 — a 5xx here means the
     * storage service is sick, not that the blob is missing.
     */
    public suspend fun headBlob(blobId: ByteArray): Long? {
        if (blobId.size != BLOB_ID_LEN) {
            throw StorageClientArgError(
                "blobId must be $BLOB_ID_LEN bytes (got ${blobId.size})",
            )
        }
        val hex = blobIdHex(blobId)
        val endpoint = "/blob/$hex"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.head(url)
        } catch (cause: Throwable) {
            throw StorageNetworkError(endpoint, cause)
        }
        val status = response.status.value
        if (status == 404) return null
        if (status !in 200..299) {
            throw StorageHttpError(
                status = status,
                endpoint = endpoint,
                body = "HEAD $endpoint returned $status",
            )
        }
        val cl = response.headers[HttpHeaders.ContentLength] ?: return null
        return cl.toLongOrNull()
    }

    public companion object {
        /** Storage service URL — Android emulator loopback
         *  alias, port `8083` matching the dev
         *  `lakko-svc-storage` bind. */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8083"

        /** Raw SHA-256 blob id length. */
        public const val BLOB_ID_LEN: Int = 32

        /** Per-blob ceiling. Mirrors `MAX_BLOB_BYTES` in
         *  `services/storage/src/main.rs` and
         *  `MAX_ATTACHMENT_BYTES` in `lakko-attachments` —
         *  surface here so a misbehaving caller fails fast
         *  without spending an oversize-body round-trip. */
        public const val MAX_BLOB_BYTES: Int = 50 * 1024 * 1024

        /**
         * Lowercase-hex encode the 32-byte blob_id. Public
         * because it's a useful helper for tests + for any
         * caller assembling storage URLs directly without going
         * through this client.
         */
        public fun blobIdHex(bytes: ByteArray): String {
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
// Helpers
// =============================================================================

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

/** Programmer error — surfaced before any network call. */
public class StorageClientArgError(message: String) : IllegalArgumentException(message)

/** Server returned a non-2xx, non-404 status. */
public class StorageHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
) : RuntimeException("storage $endpoint → $status: $body")

/** Network-level failure (DNS, TLS, no route, …). */
public class StorageNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("storage $endpoint network error: $cause", cause)
