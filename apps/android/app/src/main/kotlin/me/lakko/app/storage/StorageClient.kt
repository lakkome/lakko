// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import io.ktor.client.plugins.onUpload
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.head
import io.ktor.client.request.post
import io.ktor.client.request.header
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
    /** ADR-0065 section 5 — where the hour's upload / read pass comes from.
     *  Defaults to the process-wide passes in production and to none with a
     *  test engine (a mock answers without a pass). */
    private val passes: BlobPassSource? = if (engine == null) BlobAccess.passes else null,
    /** ADR-0065 section 6 — the account's delete keys (the upload's delete
     *  public key, the delete's signature). */
    private val deleteKeys: BlobDeleteKeys? = if (engine == null) BlobAccess.deleteKeys else null,
) : AutoCloseable {

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine)
    } else {
        HttpClient(lakkoHttpEngine())
    }

    /** Bearer token bound to the per-account `/backup` routes
     *  (Phase 2a1c). `null` is OK for the content-addressed
     *  `/blob/...` routes — they're auth-free; a `null` value on
     *  a backup-route call surfaces as a 401 from the server. */
    private var sessionToken: String? = null

    /** Bind / rebind the bearer token used for the per-account
     *  backup routes. Mirror of the web + iOS sides. */
    public fun setSessionToken(token: String?) {
        this.sessionToken = token
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
    public suspend fun putBlob(
        blobId: ByteArray,
        ciphertext: ByteArray,
        /** ADR-0047 — bytes sent so far and the body length, for the media
         *  bubble's determinate upload ring. Optional; `null` changes nothing
         *  about the request. */
        onProgress: ((sent: Long, total: Long) -> Unit)? = null,
    ) {
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
        // ADR-0065 section 6: only a device of this account can delete it.
        val deleteKey = deleteKeys?.let { keys ->
            keys.publicKey(blobId) ?: throw StorageClientArgError("no account to derive the blob's delete key from")
        }
        val response = withPass(BlobPassKind.UPLOAD, endpoint) { pass ->
            client.put(url) {
                contentType(ContentType.Application.OctetStream)
                pass?.let { header(BLOB_PASS_HEADER, it) }
                deleteKey?.let { header(BLOB_DELETE_KEY_HEADER, base64UrlNoPad(it)) }
                setBody(ciphertext)
                if (onProgress != null) {
                    onUpload { sent, length ->
                        onProgress(sent, length ?: ciphertext.size.toLong())
                    }
                }
            }
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

    /**
     * Send one blob request under a pass of [kind]: take the cached pass;
     * on a stale-pass refusal take a new one and send once more; a budget
     * refusal under a fresh pass is the person's limit ([StorageLimitError]).
     * Without a pass source the request goes out as is.
     */
    private suspend fun withPass(
        kind: BlobPassKind,
        endpoint: String,
        send: suspend (pass: String?) -> HttpResponse,
    ): HttpResponse {
        val source = passes
        if (source == null) {
            return try {
                send(null)
            } catch (cause: Throwable) {
                throw StorageNetworkError(endpoint, cause)
            }
        }
        var renewed = false
        var pass = source.pass(kind)
        while (true) {
            val response = try {
                send(pass)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                throw StorageNetworkError(endpoint, cause)
            }
            val status = response.status.value
            if (status in 200..299) return response
            val body = response.safeReadBody()
            when (classifyBlobRefusal(status, body, renewed)) {
                BlobRefusal.RENEW_PASS -> {
                    source.refused(kind, pass)
                    pass = source.pass(kind, refresh = true)
                    renewed = true
                }
                BlobRefusal.LIMIT -> {
                    source.refused(kind, pass)
                    throw StorageLimitError(kind)
                }
                BlobRefusal.GONE -> return response
                BlobRefusal.OTHER -> throw StorageHttpError(status, endpoint, body)
            }
        }
    }

    /**
     * ADR-0065 section 3 — `POST /blob/keep`: mark each existing blob in
     * [blobIds] (at most [KEEP_MAX_IDS]) accessed today. The server answers
     * `204` whether or not the ids exist.
     */
    public suspend fun keep(blobIds: List<ByteArray>) {
        require(blobIds.size in 1..KEEP_MAX_IDS) { "1..$KEEP_MAX_IDS ids per keep (got ${blobIds.size})" }
        val endpoint = "/blob/keep"
        val body = "{\"ids\":[" + blobIds.joinToString(",") { "\"${blobIdHex(it)}\"" } + "]}"
        withPass(BlobPassKind.READ, endpoint) { pass ->
            client.post("$baseUrl$endpoint") {
                pass?.let { header(BLOB_PASS_HEADER, it) }
                contentType(ContentType.Application.Json)
                setBody(body)
            }
        }
    }

    /**
     * ADR-0065 section 6 — `DELETE /blob/<hex>`, signed with the blob's
     * delete key (no pass). Returns `true` on `204`; `false` on the uniform
     * `403 blob-delete-refused` (not this account's file, or already gone).
     * Throws on anything else, so the caller keeps it for a retry.
     */
    public suspend fun deleteBlob(blobId: ByteArray): Boolean {
        if (blobId.size != BLOB_ID_LEN) {
            throw StorageClientArgError("blobId must be $BLOB_ID_LEN bytes (got ${blobId.size})")
        }
        val endpoint = "/blob/${blobIdHex(blobId)}"
        val proof = deleteKeys?.deleteProof(blobId, endpoint)
            ?: throw StorageClientArgError("no account to sign the delete with")
        val response = try {
            client.delete("$baseUrl$endpoint") { header(proof.first, proof.second) }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            throw StorageNetworkError(endpoint, cause)
        }
        val status = response.status.value
        // 204 deleted; 403 is the uniform final refusal; anything else
        // (a 404 is not expected) is tried again later.
        if (status == 204) return true
        if (status == 403) return false
        throw StorageHttpError(status, endpoint, response.safeReadBody())
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
        val response: HttpResponse = withPass(BlobPassKind.READ, endpoint) { pass ->
            client.get(url) { pass?.let { header(BLOB_PASS_HEADER, it) } }
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
        val response: HttpResponse = withPass(BlobPassKind.READ, endpoint) { pass ->
            client.head(url) { pass?.let { header(BLOB_PASS_HEADER, it) } }
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

    // -----------------------------------------------------------------
    // Per-account backup (Phase 2a1c)
    // -----------------------------------------------------------------

    /**
     * `PUT /backup` — overwrite this session's backup blob. Body
     * is the wire bytes from `sealBackup` (UniFFI). The server
     * identifies the owning account via the session token —
     * there is no account_id in the URL or body, so a stolen
     * token cannot scribble over another account.
     */
    public suspend fun putBackup(envelopeBytes: ByteArray) {
        val endpoint = "/backup"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.put(url) {
                sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
                contentType(ContentType.Application.OctetStream)
                setBody(envelopeBytes)
            }
        } catch (cause: Throwable) {
            throw StorageNetworkError("$endpoint (PUT)", cause)
        }
        val status = response.status.value
        if (status in 200..299) return
        throw StorageHttpError(
            status = status,
            endpoint = "$endpoint (PUT)",
            body = response.safeReadBody(),
        )
    }

    /** `GET /backup` — fetch this session's backup blob, or
     *  `null` when none has been uploaded yet. */
    public suspend fun getBackup(): ByteArray? {
        val endpoint = "/backup"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.get(url) {
                sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            }
        } catch (cause: Throwable) {
            throw StorageNetworkError("$endpoint (GET)", cause)
        }
        val status = response.status.value
        if (status == 404) return null
        if (status !in 200..299) {
            throw StorageHttpError(
                status = status,
                endpoint = "$endpoint (GET)",
                body = response.safeReadBody(),
            )
        }
        return response.bodyAsBytes()
    }

    /** `DELETE /backup` — drop this session's backup blob.
     *  Idempotent; the server returns 204 regardless of prior
     *  state. */
    public suspend fun deleteBackup() {
        val endpoint = "/backup"
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.delete(url) {
                sessionToken?.let { header(HttpHeaders.Authorization, "Bearer $it") }
            }
        } catch (cause: Throwable) {
            throw StorageNetworkError("$endpoint (DELETE)", cause)
        }
        val status = response.status.value
        if (status in 200..299) return
        throw StorageHttpError(
            status = status,
            endpoint = "$endpoint (DELETE)",
            body = response.safeReadBody(),
        )
    }

    public companion object {
        /** Storage service URL — `<LAKKO_API_BASE_URL>/storage`
         *  ([ApiEndpoints]), or the Android emulator loopback alias +
         *  the dev `lakko-svc-storage` bind (`http://10.0.2.2:8083`)
         *  without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.STORAGE)

        /** ADR-0065 — at most this many ids per `POST /blob/keep`. */
        public const val KEEP_MAX_IDS: Int = 256

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
