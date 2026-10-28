// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import android.content.Context
import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.util.Log
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.effect.Crop
import androidx.media3.effect.Presentation
import androidx.media3.transformer.Composition
import androidx.media3.transformer.DefaultEncoderFactory
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.Effects
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.Transformer
import androidx.media3.transformer.VideoEncoderSettings
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import me.lakko.app.profile.AvatarBounds
import me.lakko.app.profile.CropSquare
import me.lakko.app.profile.cropNdc
import me.lakko.app.profile.encodeUnderBudget
import me.lakko.app.profile.outputSide
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID
import kotlin.coroutines.resume

/**
 * docs/ux/profile-screen.md §2.2 — turning a picked or captured photo or
 * video into what the profile carries (ADR-0084 §1): a square JPEG of at
 * most 640 px and 256 KiB; for a video, a square H.264 MP4 without sound of
 * at most 480 px, 10 s and 4 MiB, plus the JPEG of its first frame.
 *
 * Every file this writes lives under the cache directory's
 * [AvatarCache.PLAY_DIR], which sign-out and forget device empty.
 */
public object AvatarMedia {
    private const val TAG = "LakkoAvatarMedia"

    /** A picked or captured source, copied into the app's cache. */
    public sealed interface Source {
        public val file: File

        /** A photo, decoded with its orientation applied. */
        public class Photo(override val file: File, public val bitmap: Bitmap) : Source

        /** A video: its display size (rotation applied), length and a strip
         *  of frames for choosing where the 10 s start. */
        public class Video(
            override val file: File,
            public val width: Int,
            public val height: Int,
            public val rotation: Int,
            public val durationMs: Long,
            public val firstFrame: Bitmap,
            public val strip: List<Pair<Long, Bitmap>>,
        ) : Source
    }

    /** The encoded result `Use` hands to the profile screen. */
    public class Encoded(
        public val still: ByteArray,
        public val stillSide: Int,
        public val stillBitmap: Bitmap,
        public val video: File? = null,
        public val videoSide: Int? = null,
        public val videoDurationMs: Int? = null,
    )

    private fun workDir(context: Context): File =
        File(context.cacheDir, AvatarCache.PLAY_DIR).also { it.mkdirs() }

    /** A fresh file under the work directory. */
    public fun newFile(context: Context, suffix: String): File =
        File(workDir(context), "edit-${UUID.randomUUID()}$suffix")

    /** Copy [uri] into the cache and open it as a photo or a video, or
     *  `null` when it is neither (or the video is not an MP4 this device
     *  plays). Off the main thread. */
    public suspend fun open(context: Context, uri: Uri): Source? = withContext(Dispatchers.IO) {
        val type = runCatching { context.contentResolver.getType(uri) }.getOrNull().orEmpty()
        val isVideo = type.startsWith("video/")
        val file = newFile(context, if (isVideo) ".mp4" else ".img")
        try {
            context.contentResolver.openInputStream(uri)?.use { input ->
                file.outputStream().use { input.copyTo(it) }
            } ?: return@withContext null
        } catch (e: Throwable) {
            Log.w(TAG, "picked item could not be read: ${e.javaClass.simpleName}")
            file.delete()
            return@withContext null
        }
        if (isVideo) {
            if (type != "video/mp4") {
                file.delete()
                return@withContext null
            }
            openVideo(file)
        } else {
            openPhoto(file)
        }
    }

    /** Open a photo file (a capture). */
    public suspend fun openPhoto(file: File): Source.Photo? = withContext(Dispatchers.IO) {
        val bitmap = me.lakko.app.media.decodeOrientedBitmap(file, SOURCE_MAX_SIDE)
        if (bitmap == null) {
            file.delete()
            null
        } else {
            Source.Photo(file, bitmap)
        }
    }

    /** Open a video file (a capture or a picked MP4). */
    public suspend fun openVideo(file: File): Source.Video? = withContext(Dispatchers.IO) {
        val retriever = MediaMetadataRetriever()
        try {
            retriever.setDataSource(file.absolutePath)
            if (retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_VIDEO) != "yes") {
                file.delete()
                return@withContext null
            }
            val w = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull() ?: 0
            val h = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull() ?: 0
            val rotation = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION)
                ?.toIntOrNull() ?: 0
            val duration = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)
                ?.toLongOrNull() ?: 0L
            val swap = rotation % 180 != 0
            val width = if (swap) h else w
            val height = if (swap) w else h
            val first = retriever.getFrameAtTime(0)?.let { scaleDown(it, SOURCE_MAX_SIDE) }
            if (width <= 0 || height <= 0 || duration <= 0L || first == null) {
                file.delete()
                return@withContext null
            }
            val strip = ArrayList<Pair<Long, Bitmap>>(STRIP_FRAMES)
            for (i in 0 until STRIP_FRAMES) {
                val at = duration * i / STRIP_FRAMES
                val frame = runCatching {
                    retriever.getFrameAtTime(at * 1000, MediaMetadataRetriever.OPTION_CLOSEST_SYNC)
                }.getOrNull() ?: continue
                strip += at to scaleDown(frame, STRIP_SIDE)
            }
            Source.Video(file, width, height, rotation, duration, first, strip)
        } catch (e: Throwable) {
            Log.w(TAG, "video did not open: ${e.javaClass.simpleName}")
            file.delete()
            null
        } finally {
            runCatching { retriever.release() }
        }
    }

    /** The still of [bitmap] cut to [square], at most 640 px and 256 KiB, or
     *  `null` when it cannot be made to fit. Off the main thread. */
    public suspend fun encodeStill(bitmap: Bitmap, square: CropSquare): Encoded? = withContext(Dispatchers.Default) {
        try {
            val cut = Bitmap.createBitmap(bitmap, square.left, square.top, square.side, square.side)
            val side = outputSide(square.side, AvatarBounds.STILL_MAX_SIDE)
            val scaled = if (side == square.side) cut else Bitmap.createScaledBitmap(cut, side, side, true)
            val bytes = encodeUnderBudget(AvatarBounds.STILL_MAX_BYTES) { quality ->
                ByteArrayOutputStream().also { scaled.compress(Bitmap.CompressFormat.JPEG, quality, it) }.toByteArray()
            } ?: return@withContext null
            Encoded(still = bytes, stillSide = side, stillBitmap = scaled)
        } catch (e: Throwable) {
            Log.w(TAG, "still did not encode: ${e.javaClass.simpleName}")
            null
        }
    }

    /**
     * Re-encode [source] to the profile video: [square] of the displayed
     * frame, at most 480 px, [lengthMs] from [startMs], no sound; then the
     * JPEG of its first frame. `null` when the export fails or the result
     * does not fit the ADR bounds (the screen then says the video cannot be
     * used).
     *
     * The crop is given in display coordinates (the rotation applied);
     * Media3 applies the input's rotation before the effects. That is the
     * documented behaviour; it has not been observed on a device in this
     * change.
     */
    @androidx.annotation.OptIn(UnstableApi::class)
    public suspend fun encodeVideo(
        context: Context,
        source: Source.Video,
        square: CropSquare,
        startMs: Long,
        lengthMs: Long,
    ): Encoded? {
        val side = outputSide(square.side, AvatarBounds.VIDEO_MAX_SIDE).let { it - it % 2 }.coerceAtLeast(2)
        val out = newFile(context, ".mp4")
        val ndc = cropNdc(source.width, source.height, square)
        val ok = withContext(Dispatchers.Main) {
            suspendCancellableCoroutine { cont ->
                val item = MediaItem.Builder()
                    .setUri(Uri.fromFile(source.file))
                    .setClippingConfiguration(
                        MediaItem.ClippingConfiguration.Builder()
                            .setStartPositionMs(startMs)
                            .setEndPositionMs(startMs + lengthMs)
                            .build(),
                    )
                    .build()
                val edited = EditedMediaItem.Builder(item)
                    .setRemoveAudio(true)
                    .setEffects(
                        Effects(
                            emptyList(),
                            listOf(
                                Crop(ndc[0], ndc[1], ndc[2], ndc[3]),
                                Presentation.createForWidthAndHeight(side, side, Presentation.LAYOUT_SCALE_TO_FIT),
                            ),
                        ),
                    )
                    .build()
                val transformer = Transformer.Builder(context)
                    .setVideoMimeType(MimeTypes.VIDEO_H264)
                    .setEncoderFactory(
                        DefaultEncoderFactory.Builder(context)
                            .setRequestedVideoEncoderSettings(
                                VideoEncoderSettings.Builder().setBitrate(VIDEO_BITRATE).build(),
                            )
                            .build(),
                    )
                    .addListener(
                        object : Transformer.Listener {
                            override fun onCompleted(composition: Composition, exportResult: ExportResult) {
                                if (cont.isActive) cont.resume(true)
                            }

                            override fun onError(
                                composition: Composition,
                                exportResult: ExportResult,
                                exportException: ExportException,
                            ) {
                                Log.w(TAG, "profile video export failed: ${exportException.errorCodeName}")
                                if (cont.isActive) cont.resume(false)
                            }
                        },
                    )
                    .build()
                cont.invokeOnCancellation { runCatching { transformer.cancel() } }
                try {
                    transformer.start(edited, out.absolutePath)
                } catch (e: Throwable) {
                    Log.w(TAG, "profile video export did not start: ${e.javaClass.simpleName}")
                    if (cont.isActive) cont.resume(false)
                }
            }
        }
        if (!ok || !out.exists() || out.length() !in 1..AvatarBounds.VIDEO_MAX_BYTES.toLong()) {
            out.delete()
            return null
        }
        return withContext(Dispatchers.IO) { finishVideo(out) }
    }

    private suspend fun finishVideo(out: File): Encoded? {
        val retriever = MediaMetadataRetriever()
        try {
            retriever.setDataSource(out.absolutePath)
            if (retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_AUDIO) == "yes") {
                Log.w(TAG, "profile video kept a sound track")
                out.delete()
                return null
            }
            val w = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull() ?: 0
            val h = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull() ?: 0
            val duration = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)
                ?.toLongOrNull() ?: 0L
            if (w != h || w !in 1..AvatarBounds.VIDEO_MAX_SIDE) {
                Log.w(TAG, "profile video is not a square within the bound")
                out.delete()
                return null
            }
            val ms = duration.coerceIn(1L, AvatarBounds.VIDEO_MAX_MS.toLong()).toInt()
            val frame = retriever.getFrameAtTime(0) ?: run {
                out.delete()
                return null
            }
            val side = minOf(frame.width, frame.height)
            val still = encodeStill(frame, CropSquare((frame.width - side) / 2, (frame.height - side) / 2, side))
                ?: run {
                    out.delete()
                    return null
                }
            return Encoded(
                still = still.still,
                stillSide = still.stillSide,
                stillBitmap = still.stillBitmap,
                video = out,
                videoSide = w,
                videoDurationMs = ms,
            )
        } catch (e: Throwable) {
            Log.w(TAG, "profile video did not check out: ${e.javaClass.simpleName}")
            out.delete()
            return null
        } finally {
            runCatching { retriever.release() }
        }
    }

    private fun scaleDown(bitmap: Bitmap, maxSide: Int): Bitmap {
        val longest = maxOf(bitmap.width, bitmap.height)
        if (longest <= maxSide) return bitmap
        val f = maxSide.toFloat() / longest
        return Bitmap.createScaledBitmap(
            bitmap,
            (bitmap.width * f).toInt().coerceAtLeast(1),
            (bitmap.height * f).toInt().coerceAtLeast(1),
            true,
        )
    }

    /** The longest side a picked photo is decoded at for cropping. */
    private const val SOURCE_MAX_SIDE = 2048
    private const val STRIP_FRAMES = 8
    private const val STRIP_SIDE = 96

    /** 1.2 Mbit/s: 10 s is about 1.5 MB, under the 4 MiB bound. Chosen, not
     *  measured against a picture quality target. */
    private const val VIDEO_BITRATE = 1_200_000
}
