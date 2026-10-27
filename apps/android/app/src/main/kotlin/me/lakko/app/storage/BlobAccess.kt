// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.HttpClient
import io.ktor.client.request.post
import io.ktor.client.request.header
import io.ktor.client.statement.bodyAsText
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.errorCodeOf
import me.lakko.app.anon.isAnonTokenQuotaExhausted
import me.lakko.app.anon.sendWithToken
import me.lakko.app.transport.lakkoHttpEngine

/*
 * ADR-0065 section 5 — the gate on the blob store: one anonymous token buys
 * an hour's pass (upload: files and bytes; read: downloads and keeps), sent
 * as `x-lakko-blob-pass` on every PUT / GET / HEAD / keep. Section 6 — every
 * upload carries the blob's delete public key, derived from the account
 * identity, and `DELETE /blob/{hex}` is signed with it.
 *
 * The decisions (when a cached pass is still good, what a refusal means) are
 * pure and run on the host JVM; the network halves are behind interfaces.
 */

/** The two passes. */
public enum class BlobPassKind(public val path: String, public val context: AnonTokenContext) {
    UPLOAD("upload", AnonTokenContext.BLOB_UPLOAD),
    READ("read", AnonTokenContext.BLOB_READ),
}

/** A pass as the server issued it. */
public data class BlobPass(val value: String, val kind: BlobPassKind, val expiresAtMs: Long) {
    override fun toString(): String = "BlobPass($kind, expiresAtMs=$expiresAtMs)"
}

/** The pass request header. */
public const val BLOB_PASS_HEADER: String = "x-lakko-blob-pass"

/** The upload's delete-key header. */
public const val BLOB_DELETE_KEY_HEADER: String = "x-lakko-blob-delete-key"

/**
 * One cached pass per kind, per process. A pass is used until [MARGIN_MS]
 * before its expiry, so a request never leaves with a pass that runs out on
 * the way; [invalidate] drops one the server refused.
 */
public class BlobPassCache(private val now: () -> Long = System::currentTimeMillis) {
    private val passes = HashMap<BlobPassKind, BlobPass>()

    @Synchronized
    public fun current(kind: BlobPassKind): BlobPass? {
        val held = passes[kind] ?: return null
        if (held.expiresAtMs - MARGIN_MS <= now()) {
            passes.remove(kind)
            return null
        }
        return held
    }

    @Synchronized
    public fun put(pass: BlobPass) {
        passes[pass.kind] = pass
    }

    /** Drop [value] if it is still the cached pass (a concurrent refresh may
     *  already have replaced it). */
    @Synchronized
    public fun invalidate(kind: BlobPassKind, value: String) {
        if (passes[kind]?.value == value) passes.remove(kind)
    }

    @Synchronized
    public fun clear() {
        passes.clear()
    }

    public companion object {
        /** Chosen, not measured: a minute of slack before the hour runs out. */
        public const val MARGIN_MS: Long = 60_000L
    }
}

/** What a refused blob request means for the caller. */
public enum class BlobRefusal {
    /** The pass is stale (expired, invalid, a server restart dropped its
     *  count, or its budget ran out): take a new pass once and retry. */
    RENEW_PASS,

    /** 404 `blob-not-found`: the file is gone (UX 4.1, no Retry). */
    GONE,

    /** The person's limit is reached (UX 4.2). */
    LIMIT,

    /** Anything else, a nginx 429 with an HTML body included: the generic
     *  failure with Try again. */
    OTHER,
}

/** Classify a blob response. [renewed] says whether this answer already came
 *  with a fresh pass: a budget refusal then is the limit, not a stale pass. */
public fun classifyBlobRefusal(status: Int, body: String?, renewed: Boolean): BlobRefusal {
    val code = errorCodeOf(body)
    return when {
        status == 404 -> BlobRefusal.GONE
        status == 401 && code in PASS_STALE_CODES -> if (renewed) BlobRefusal.OTHER else BlobRefusal.RENEW_PASS
        status == 429 && code in BUDGET_CODES -> if (renewed) BlobRefusal.LIMIT else BlobRefusal.RENEW_PASS
        else -> BlobRefusal.OTHER
    }
}

private val PASS_STALE_CODES = setOf("blob-pass-required", "blob-pass-invalid", "blob-pass-expired")
private val BUDGET_CODES = setOf("blob-upload-budget-exhausted", "blob-read-budget-exhausted")

/** The person's limit for sending or opening files is reached (UX 4.2),
 *  either the hour's pass budget or the day's tokens. */
public class StorageLimitError(public val kind: BlobPassKind, cause: Throwable? = null) :
    RuntimeException("blob ${kind.path} limit reached", cause)

/** Parse `POST /blob/pass/{kind}`'s answer. */
public fun parseBlobPass(kind: BlobPassKind, body: String): BlobPass? = try {
    val obj = Json.parseToJsonElement(body) as? JsonObject
    val value = obj?.get("pass")?.jsonPrimitive?.content
    val expires = obj?.get("expires_at_unix_secs")?.jsonPrimitive?.longOrNull
    if (value.isNullOrEmpty() || expires == null) null else BlobPass(value, kind, expires * 1000L)
} catch (_: Throwable) {
    null
}

/** Where a request takes its pass. */
public interface BlobPassSource {
    /** A live pass of [kind]; [refresh] forces a new one. @throws
     *  StorageLimitError when no token can be had for the day. */
    public suspend fun pass(kind: BlobPassKind, refresh: Boolean = false): String

    /** The server refused [value]. */
    public fun refused(kind: BlobPassKind, value: String)
}

/** The process-wide passes, bought from the token pool. */
public class IssuedBlobPasses(
    private val baseUrl: () -> String = { StorageClient.DEFAULT_BASE_URL },
    private val tokens: AnonTokenSource = AnonTokens.shared,
    private val cache: BlobPassCache = BlobPassCache(),
) : BlobPassSource {
    private val locks = BlobPassKind.entries.associateWith { Mutex() }
    private val client: HttpClient by lazy { HttpClient(lakkoHttpEngine()) }

    override suspend fun pass(kind: BlobPassKind, refresh: Boolean): String =
        locks.getValue(kind).withLock {
            if (!refresh) cache.current(kind)?.let { return@withLock it.value }
            val issued = buy(kind)
            cache.put(issued)
            issued.value
        }

    override fun refused(kind: BlobPassKind, value: String) {
        cache.invalidate(kind, value)
    }

    private suspend fun buy(kind: BlobPassKind): BlobPass {
        val endpoint = "/blob/pass/${kind.path}"
        val response = try {
            tokens.sendWithToken(kind.context) { name, value ->
                client.post("${baseUrl()}$endpoint") { header(name, value) }
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            // The day's tokens are used up: that is the person's limit.
            if (isAnonTokenQuotaExhausted(cause)) throw StorageLimitError(kind, cause)
            throw StorageNetworkError(endpoint, cause)
        }
        val text = runCatching { response.bodyAsText() }.getOrDefault("")
        if (response.status.value != 200) throw StorageHttpError(response.status.value, endpoint, text.take(512))
        return parseBlobPass(kind, text) ?: throw StorageHttpError(200, endpoint, "unreadable pass")
    }

    /** Sign-out / forget device: no pass outlives its session. */
    public fun clear() {
        cache.clear()
    }
}

/** The delete key of a blob, and the signature of its delete. */
public interface BlobDeleteKeys {
    /** The 32-byte delete public key of [blobId], or `null` with no account. */
    public fun publicKey(blobId: ByteArray): ByteArray?

    /** The `x-lakko-storage-proof` header (name, value) for
     *  `DELETE /blob/<hex>`, or `null` with no account. */
    public fun deleteProof(blobId: ByteArray, path: String): Pair<String, String>?
}

/**
 * The blob delete keys (ADR-0065 section 6), from the account's LINEAGE seed
 * (ADR-0074: the earliest own predecessor's seed when the account rotated,
 * else the current one; [blobDeleteSeedOf]), so a rotation keeps every file
 * uploaded before it deletable. Rebuilt when the seed changes. Only the blob
 * delete keys use this; the ADR-0064 vault keys are not built here.
 */
public object AccountBlobDeleteKeys : BlobDeleteKeys {
    @Volatile
    private var seedSource: () -> ByteArray? = { null }

    private var cachedFor: Int? = null
    private var keys: uniffi.lakko_ffi_uniffi.VaultKeys? = null

    /** Where the account identity seed comes from (the session). */
    public fun install(source: () -> ByteArray?) {
        seedSource = source
    }

    @Synchronized
    private fun keys(): uniffi.lakko_ffi_uniffi.VaultKeys? {
        val seed = seedSource() ?: return null
        val tag = seed.contentHashCode()
        try {
            return keysFor(seed, tag)
        } finally {
            seed.fill(0)
        }
    }

    private fun keysFor(seed: ByteArray, tag: Int): uniffi.lakko_ffi_uniffi.VaultKeys? {
        if (tag == cachedFor && keys != null) return keys
        keys?.close()
        keys = try {
            me.lakko.app.auth.SessionSecretWipe.identity(seed.copyOf()).use { uniffi.lakko_ffi_uniffi.vaultKeys(it) }
        } catch (_: Throwable) {
            null
        }
        cachedFor = tag
        return keys
    }

    override fun publicKey(blobId: ByteArray): ByteArray? = try {
        keys()?.blobDeletePublicKey(blobId)
    } catch (_: Throwable) {
        null
    }

    override fun deleteProof(blobId: ByteArray, path: String): Pair<String, String>? = try {
        val k = keys() ?: null
        k?.let { uniffi.lakko_ffi_uniffi.storageProofHeaderName() to it.signBlobDelete(blobId, "DELETE", path, ByteArray(0)) }
    } catch (_: Throwable) {
        null
    }

    /** Sign-out / forget device. */
    @Synchronized
    public fun clear() {
        keys?.close()
        keys = null
        cachedFor = null
    }
}

/** The seed the blob delete keys derive from: the earliest own predecessor's
 *  ([predecessorSeed]) when the account rotated, else [currentSeed]. */
public fun blobDeleteSeedOf(predecessorSeed: ByteArray?, currentSeed: ByteArray?): ByteArray? =
    predecessorSeed ?: currentSeed

/** The shared defaults the storage client uses. */
public object BlobAccess {
    public val passes: IssuedBlobPasses = IssuedBlobPasses()
    public val deleteKeys: BlobDeleteKeys = AccountBlobDeleteKeys

    /** Drop the passes and the cached keys (sign-out, forget device). */
    public fun clear() {
        passes.clear()
        AccountBlobDeleteKeys.clear()
    }
}

/** base64url without padding (the delete-key header). */
internal fun base64UrlNoPad(bytes: ByteArray): String =
    java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
