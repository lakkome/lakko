// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import android.media.MediaMetadataRetriever
import android.util.Log
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream

/**
 * UX spec §10 Sending — what a picked photo or video carries next to the
 * original: its pixel size (scaled to fit 4096) and a small JPEG thumb.
 * [thumb] is `null` when no preview could be produced; the send goes ahead
 * without it.
 */
public class VisualPrep(
    public val size: PixelSize?,
    public val thumb: ByteArray?,
    public val thumbSize: PixelSize?,
)

private const val TAG = "LakkoVisualMedia"

/**
 * Build the [VisualPrep] for a picked attachment, or `null` when its MIME type
 * is not an allowlisted image or video (a plain file). Never throws; runs the
 * decoders, so call it off the main thread.
 */
public fun prepareVisualMedia(
    bytes: ByteArray,
    mime: String?,
    /** Where a video is written for [MediaMetadataRetriever], which reads a
     *  file path far more reliably than an in-memory data source. */
    scratchDir: File,
    allowlist: MediaAllowlist = coreAllowlist,
): VisualPrep? = when (inlineKindOf(mime, allowlist)) {
    InlineKind.IMAGE -> runCatching { prepareImage(bytes) }.getOrElse {
        Log.w(TAG, "photo preview failed: ${it.javaClass.simpleName}")
        VisualPrep(null, null, null)
    }
    InlineKind.VIDEO -> runCatching { prepareVideo(bytes, scratchDir) }.getOrElse {
        Log.w(TAG, "video preview failed: ${it.javaClass.simpleName}")
        VisualPrep(null, null, null)
    }
    else -> null
}

private fun prepareImage(bytes: ByteArray): VisualPrep {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    val rotation = exifRotation { ExifInterface(ByteArrayInputStream(bytes)) }
    val raw = orientedSize(bounds.outWidth, bounds.outHeight, rotation)
    val size = raw?.let { fitWithin(it.width, it.height, MEDIA_DIMENSION_CAP) }
    val bitmap = if (raw == null) {
        null
    } else {
        BitmapFactory.decodeByteArray(
            bytes,
            0,
            bytes.size,
            BitmapFactory.Options().apply {
                inSampleSize = sampleSizeFor(bounds.outWidth, bounds.outHeight, THUMB_LONGEST_SIDE)
            },
        )
    }
    val (thumb, thumbSize) = bitmap?.let { encodeThumb(rotate(it, rotation)) } ?: (null to null)
    return VisualPrep(size, thumb, thumbSize)
}

private fun prepareVideo(bytes: ByteArray, scratchDir: File): VisualPrep {
    val dir = File(scratchDir, "visual-prep").apply { mkdirs() }
    val scratch = File(dir, "${java.util.UUID.randomUUID()}.mp4")
    scratch.writeBytes(bytes)
    val retriever = MediaMetadataRetriever()
    try {
        retriever.setDataSource(scratch.absolutePath)
        val w = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull()
        val h = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull()
        val rotation = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION)
            ?.toIntOrNull() ?: 0
        val size = if (w != null && h != null) {
            orientedSize(w, h, rotation)?.let { fitWithin(it.width, it.height, MEDIA_DIMENSION_CAP) }
        } else {
            null
        }
        // The retriever returns the frame already rotated for display.
        val frame = runCatching {
            retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST_SYNC)
        }.getOrNull()
        val (thumb, thumbSize) = frame?.let { encodeThumb(it) } ?: (null to null)
        return VisualPrep(size, thumb, thumbSize)
    } finally {
        runCatching { retriever.release() }
        scratch.delete()
    }
}

private fun orientedSize(width: Int, height: Int, rotation: Int): PixelSize? {
    if (width <= 0 || height <= 0) return null
    return if (rotationSwapsSides(rotation)) PixelSize(height, width) else PixelSize(width, height)
}

private fun encodeThumb(source: Bitmap): Pair<ByteArray?, PixelSize?> {
    val target = fitWithin(source.width, source.height, THUMB_LONGEST_SIDE) ?: return null to null
    val scaled = if (target.width == source.width && target.height == source.height) {
        source
    } else {
        Bitmap.createScaledBitmap(source, target.width, target.height, true)
    }
    val bytes = compressUnderBudget { quality ->
        ByteArrayOutputStream().also { out ->
            scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)
        }.toByteArray()
    }
    return if (bytes == null) null to null else bytes to target
}

private fun exifRotation(open: () -> ExifInterface): Int = runCatching {
    when (open().getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)) {
        ExifInterface.ORIENTATION_ROTATE_90, ExifInterface.ORIENTATION_TRANSPOSE -> 90
        ExifInterface.ORIENTATION_ROTATE_180 -> 180
        ExifInterface.ORIENTATION_ROTATE_270, ExifInterface.ORIENTATION_TRANSVERSE -> 270
        else -> 0
    }
}.getOrDefault(0)

private fun rotate(bitmap: Bitmap, degrees: Int): Bitmap {
    if (degrees % 360 == 0) return bitmap
    val m = Matrix().apply { postRotate(degrees.toFloat()) }
    return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, m, true)
}

/** The largest power-of-two sample size that keeps the longest side at or
 *  above [minLongest]. */
private fun sampleSizeFor(width: Int, height: Int, minLongest: Int): Int {
    var sample = 1
    val longest = maxOf(width, height)
    while (longest / (sample * 2) >= minLongest) sample *= 2
    return sample
}

/**
 * Decode an image file for display with its EXIF orientation applied, the
 * longest side at most about [maxSide] px. `null` on any failure; never
 * throws. Call off the main thread.
 */
public fun decodeOrientedBitmap(file: File, maxSide: Int): Bitmap? = try {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.absolutePath, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) {
        null
    } else {
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / sample > maxSide * 2) sample *= 2
        val bitmap = BitmapFactory.decodeFile(
            file.absolutePath,
            BitmapFactory.Options().apply { inSampleSize = sample },
        )
        val rotation = exifRotation { FileInputStream(file).use { ExifInterface(it) } }
        bitmap?.let { rotate(it, rotation) }
    }
} catch (e: Throwable) {
    // OutOfMemoryError included: a hostile size must not crash the chat.
    Log.w(TAG, "image did not decode: ${e.javaClass.simpleName}")
    null
}
