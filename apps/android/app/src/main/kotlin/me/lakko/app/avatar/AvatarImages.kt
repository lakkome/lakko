// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadataRetriever
import android.util.Log
import android.util.LruCache
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext
import me.lakko.app.profile.AvatarBlob
import me.lakko.app.profile.AvatarBounds
import me.lakko.app.profile.blobId
import me.lakko.app.profile.key
import me.lakko.app.profile.nonce
import me.lakko.app.storage.StorageClient
import java.io.File

/**
 * ADR-0084 §3 — downloading, checking and handing out profile photos.
 *
 * Foreground only: the one caller is the avatar component
 * ([me.lakko.app.ui.components.LakkoAvatar]) and the profile screen. The
 * background notification paths never reach this object and keep showing
 * initials.
 *
 * A blob is fetched at most once at a time (a second display waits for the
 * first), checked (its size against the reference and the ADR bound, then
 * that the still decodes as an image and the video opens as an MP4 with a
 * video track), sealed into the [AvatarCache], and only then shown. Anything
 * else is dropped and marked failed for a minute; the initials stay.
 */
public object AvatarImages {
    private const val TAG = "LakkoAvatarImages"

    @Volatile
    private var cacheInstance: AvatarCache? = null

    private val inflight = HashMap<String, CompletableDeferred<ByteArray?>>()
    private val bitmaps = LruCache<String, Bitmap>(48)

    private val mutableRevision = MutableStateFlow(0L)

    /** Moves when a photo arrived, so a screen showing initials redraws. */
    public val revision: StateFlow<Long> = mutableRevision

    /** The device's cache, created on first use. */
    public fun cache(context: Context): AvatarCache {
        cacheInstance?.let { return it }
        return synchronized(this) {
            cacheInstance ?: AvatarCache(
                dir = File(context.applicationContext.noBackupFilesDir, AvatarCache.DIR),
                seal = KeystoreAvatarSeal,
            ).also { cacheInstance = it }
        }
    }

    private val gone: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** The server answered that [blobIdHex] is gone (expired or deleted). */
    public fun isGone(blobIdHex: String): Boolean = blobIdHex in gone

    /** A still decoded from memory, without any I/O, or `null`. */
    public fun cachedBitmap(blob: AvatarBlob): Bitmap? = bitmaps[blob.blobIdHex]

    /**
     * The still for [blob]: from memory, the disk cache, or a download.
     * `null` when it is not available now (failed within the last minute,
     * or it does not check out).
     */
    public suspend fun still(context: Context, blob: AvatarBlob): Bitmap? {
        bitmaps[blob.blobIdHex]?.let { return it }
        val bytes = bytes(context, blob, isVideo = false) ?: return null
        val bitmap = withContext(Dispatchers.Default) { decodeStill(bytes, blob.side) }
        if (bitmap == null) {
            cache(context).markFailed(blob.blobIdHex)
            return null
        }
        bitmaps.put(blob.blobIdHex, bitmap)
        return bitmap
    }

    /**
     * A playable file of the profile video [blob] (MediaPlayer takes a
     * path), or `null`. Written to the cache directory and removed at
     * sign-out and forget device ([AvatarCache.PLAY_DIR]).
     */
    public suspend fun videoFile(context: Context, blob: AvatarBlob): File? {
        val out = playFile(context, blob)
        if (out != null && out.exists() && out.length() == blob.size) return out
        val bytes = bytes(context, blob, isVideo = true) ?: return null
        return withContext(Dispatchers.IO) {
            try {
                out?.parentFile?.mkdirs()
                out?.writeBytes(bytes)
                out
            } catch (_: Throwable) {
                null
            }
        }
    }

    /** Keep the bytes of a photo or video this device just uploaded, so the
     *  owner's own avatar is never downloaded again. */
    public fun putOwn(context: Context, blob: AvatarBlob, bytes: ByteArray) {
        cache(context).put(blob.blobIdHex, bytes)
        mutableRevision.update { it + 1 }
    }

    /** Forget every cached avatar (forget device, sign-out of the session). */
    public fun clear(context: Context) {
        cache(context).clear()
        bitmaps.evictAll()
        File(context.cacheDir, AvatarCache.PLAY_DIR).deleteRecursively()
        mutableRevision.update { it + 1 }
    }

    private fun playFile(context: Context, blob: AvatarBlob): File? {
        if (!AvatarCache.isBlobIdHex(blob.blobIdHex)) return null
        return File(File(context.cacheDir, AvatarCache.PLAY_DIR), "${blob.blobIdHex}.mp4")
    }

    private suspend fun bytes(context: Context, blob: AvatarBlob, isVideo: Boolean): ByteArray? {
        val cache = cache(context)
        withContext(Dispatchers.IO) { cache.get(blob.blobIdHex) }?.let { return it }
        if (!cache.mayTry(blob.blobIdHex)) return null
        val (wait, owner) = synchronized(inflight) {
            val existing = inflight[blob.blobIdHex]
            if (existing != null) {
                existing to false
            } else {
                val fresh = CompletableDeferred<ByteArray?>()
                inflight[blob.blobIdHex] = fresh
                fresh to true
            }
        }
        if (!owner) return wait.await()
        val result = try {
            download(context, blob, isVideo)
        } catch (e: Throwable) {
            if (e is kotlinx.coroutines.CancellationException) {
                wait.complete(null)
                synchronized(inflight) { inflight.remove(blob.blobIdHex) }
                throw e
            }
            null
        }
        if (result == null) {
            cache.markFailed(blob.blobIdHex)
        } else {
            withContext(Dispatchers.IO) { cache.put(blob.blobIdHex, result) }
            mutableRevision.update { it + 1 }
        }
        wait.complete(result)
        synchronized(inflight) { inflight.remove(blob.blobIdHex) }
        return result
    }

    private suspend fun download(context: Context, blob: AvatarBlob, isVideo: Boolean): ByteArray? {
        val maxBytes = if (isVideo) AvatarBounds.VIDEO_MAX_BYTES else AvatarBounds.STILL_MAX_BYTES
        if (blob.size !in 1..maxBytes.toLong()) return null
        val id = blob.blobId() ?: return null
        val key = blob.key() ?: return null
        val nonce = blob.nonce() ?: return null
        val ciphertext = withContext(Dispatchers.IO) {
            StorageClient().use { it.getBlob(id) }
        } ?: run {
            // 404: the file expired or was deleted (ADR-0087 4.5.2 "gone").
            gone.add(blob.blobIdHex)
            mutableRevision.update { it + 1 }
            return null
        }
        // The AEAD tag is 16 bytes; anything longer than the bound allows is
        // refused before it is opened.
        if (ciphertext.size.toLong() != blob.size + AEAD_TAG_BYTES) return null
        val plain = try {
            withContext(Dispatchers.Default) {
                uniffi.lakko_ffi_uniffi.decryptAttachment(ciphertext = ciphertext, key = key, nonce = nonce)
            }
        } catch (e: Throwable) {
            Log.w(TAG, "avatar did not open: ${e.javaClass.simpleName}")
            return null
        }
        if (plain.size.toLong() != blob.size) return null
        val ok = withContext(Dispatchers.Default) {
            if (isVideo) checkVideo(context, plain) else decodeStill(plain, blob.side) != null
        }
        if (!ok) {
            Log.w(TAG, "avatar did not check out as ${if (isVideo) "a video" else "an image"}")
            return null
        }
        return plain
    }

    private fun decodeStill(bytes: ByteArray, side: Int): Bitmap? = try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0 ||
            bounds.outWidth > AvatarBounds.STILL_MAX_SIDE || bounds.outHeight > AvatarBounds.STILL_MAX_SIDE ||
            bounds.outMimeType != "image/jpeg"
        ) {
            null
        } else {
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
        }
    } catch (_: Throwable) {
        null
    }

    private fun checkVideo(context: Context, bytes: ByteArray): Boolean {
        val scratch = File(context.cacheDir, "${AvatarCache.PLAY_DIR}/check-${System.nanoTime()}.mp4")
        val retriever = MediaMetadataRetriever()
        return try {
            scratch.parentFile?.mkdirs()
            scratch.writeBytes(bytes)
            retriever.setDataSource(scratch.absolutePath)
            val hasVideo = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_VIDEO) == "yes"
            val mime = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_MIMETYPE).orEmpty()
            hasVideo && mime.contains("mp4")
        } catch (_: Throwable) {
            false
        } finally {
            runCatching { retriever.release() }
            scratch.delete()
        }
    }

    private const val AEAD_TAG_BYTES = 16L
}
