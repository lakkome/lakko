// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.R
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.PreviewThumb
import me.lakko.app.media.InlineKind
import me.lakko.app.media.coreAllowlist
import me.lakko.app.media.inlineKindOf
import me.lakko.app.ui.components.LkIcon
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * docs/ux/media-messages.md section 11 — the small decrypted thumbnails of
 * the conversation list: the attachment's own sent thumb, fetched and
 * AEAD-opened when the row is drawn, kept in memory only (never written),
 * decoded small. A thumb that failed is not fetched again in this process;
 * its row keeps the neutral placeholder.
 */
internal object RowThumbs {
    private const val DECODE_SIDE = 96
    private val cache = LruCache<String, ImageBitmap>(64)
    private val failed = java.util.Collections.synchronizedSet(HashSet<String>())
    private val inflight = HashMap<String, CompletableDeferred<ImageBitmap?>>()
    private val storage by lazy { me.lakko.app.storage.StorageClient() }

    fun cached(key: String): ImageBitmap? = cache.get(key)

    suspend fun load(key: String, thumb: ThumbRef): ImageBitmap? {
        cache.get(key)?.let { return it }
        if (key in failed) return null
        if (inlineKindOf(thumb.mime, coreAllowlist) != InlineKind.IMAGE) return null
        val (deferred, owner) = synchronized(inflight) {
            inflight[key]?.let { it to false } ?: CompletableDeferred<ImageBitmap?>().also { inflight[key] = it }.let { it to true }
        }
        if (!owner) return deferred.await()
        val result = try {
            withContext(Dispatchers.IO) {
                val bytes = AttachmentBridge.downloadAndDecryptThumb(thumb, storage)
                decodeSmall(bytes, DECODE_SIDE)?.asImageBitmap()
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            synchronized(inflight) { inflight.remove(key) }
            deferred.complete(null)
            throw e
        } catch (_: Throwable) {
            null
        }
        if (result == null) failed += key else cache.put(key, result)
        synchronized(inflight) { inflight.remove(key) }
        deferred.complete(result)
        return result
    }

    /** Decode [bytes] with the longest side about [side] px; `null` on failure. */
    fun decodeSmall(bytes: ByteArray, side: Int): Bitmap? = runCatching {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= side) sample *= 2
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
    }.getOrNull()
}

/**
 * Section 11 — the row's thumbnail: 20 dp, corner radius 4, cropped to fill,
 * a small play glyph on a video's; with several items, one or two offset
 * edges behind it (2 dp right and up each) in the row's muted tone. Until the
 * thumb is there, and when it cannot be had, the neutral placeholder.
 */
@Composable
internal fun RowPreviewThumb(thumb: PreviewThumb) {
    val key = thumb.key
    var bitmap by remember(key) { mutableStateOf(key?.let(RowThumbs::cached)) }
    LaunchedEffect(key) {
        val ref = thumb.thumb ?: return@LaunchedEffect
        if (key == null || bitmap != null) return@LaunchedEffect
        bitmap = RowThumbs.load(key, ref)
    }
    val edges = thumb.stackEdges
    val muted = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.35f)
    val shape = RoundedCornerShape(4.dp)
    Box(Modifier.size((20 + 2 * edges).dp)) {
        // Back to front: the farthest edge first.
        for (i in edges downTo 1) {
            Box(
                Modifier
                    .align(Alignment.BottomStart)
                    .offset(x = (2 * i).dp, y = (-2 * i).dp)
                    .size(20.dp)
                    .clip(shape)
                    .background(muted),
            )
        }
        Box(
            Modifier
                .align(Alignment.BottomStart)
                .size(20.dp)
                .clip(shape)
                .background(MaterialTheme.colorScheme.surfaceVariant),
        ) {
            bitmap?.let { Image(it, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.size(20.dp)) }
            if (thumb.video) {
                LkIcon(
                    R.drawable.ic_lk_play_fill,
                    contentDescription = null,
                    tint = Color.White,
                    size = 10.dp,
                    modifier = Modifier.align(Alignment.Center),
                )
            }
        }
    }
}
