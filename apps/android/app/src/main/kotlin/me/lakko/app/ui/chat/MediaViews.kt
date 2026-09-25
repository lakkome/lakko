// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.SurfaceTexture
import android.graphics.drawable.AnimatedImageDrawable
import android.graphics.drawable.Drawable
import android.media.MediaPlayer
import android.os.Build
import android.util.Log
import android.view.Surface
import android.view.TextureView
import android.widget.ImageView
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.onLongClick
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.setProgress
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.media.waveformBars
import java.io.File
import java.nio.ByteBuffer

private const val TAG = "LakkoMediaViews"

/** Longest side an animation is decoded at — a bubble or grid tile never
 *  needs more, and a sender-chosen 4096 px GIF would otherwise decode every
 *  frame at full size. */
private const val MAX_DECODE_SIDE = 720

/**
 * An animated GIF / WebP that actually animates (UX spec §5 timeline: "it must
 * actually animate, not the first frame only").
 *
 * API 28+ decodes through `ImageDecoder` into an `AnimatedImageDrawable`,
 * looping, muted by nature. This app's minimum is API 26, where that decoder
 * does not exist; there the first frame is shown via `BitmapFactory` — the one
 * honest fallback without adding an image-loading dependency.
 *
 * The caller has already passed the MIME allowlist; a buffer that still fails
 * to decode yields [onFailed] and nothing is drawn (never a broken-image icon).
 */
@Composable
internal fun AnimatedImage(
    source: AnimatedSource,
    contentDescription: String,
    modifier: Modifier = Modifier,
    /** Letterbox instead of cropping (the full-screen viewer). */
    fit: Boolean = false,
    onFailed: () -> Unit = {},
) {
    var drawable by remember(source) { mutableStateOf<Drawable?>(null) }
    val failed by rememberUpdatedState(onFailed)
    LaunchedEffect(source) {
        val decoded = withContext(Dispatchers.IO) { decodeAnimated(source) }
        if (decoded == null) failed() else drawable = decoded
    }
    DisposableEffect(source) {
        onDispose {
            // `AnimatedImageDrawable` does not exist below API 28; the class
            // must not be touched there, not even by a cast.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                (drawable as? AnimatedImageDrawable)?.stop()
            }
            drawable = null
        }
    }
    val d = drawable ?: return
    AndroidView(
        factory = { ctx ->
            ImageView(ctx).apply {
                scaleType = if (fit) ImageView.ScaleType.FIT_CENTER else ImageView.ScaleType.CENTER_CROP
                this.contentDescription = contentDescription
            }
        },
        update = { view ->
            if (view.drawable !== d) {
                view.setImageDrawable(d)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && d is AnimatedImageDrawable) {
                    d.repeatCount = AnimatedImageDrawable.REPEAT_INFINITE
                    d.start()
                }
            }
        },
        modifier = modifier,
    )
}

internal sealed interface AnimatedSource {
    data class OfFile(val file: File) : AnimatedSource

    class OfBytes(val key: String, val bytes: ByteArray) : AnimatedSource {
        override fun equals(other: Any?): Boolean = other is OfBytes && other.key == key
        override fun hashCode(): Int = key.hashCode()
    }
}

private fun decodeAnimated(source: AnimatedSource): Drawable? = try {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
        val src = when (source) {
            is AnimatedSource.OfFile -> ImageDecoder.createSource(source.file)
            is AnimatedSource.OfBytes -> ImageDecoder.createSource(ByteBuffer.wrap(source.bytes))
        }
        ImageDecoder.decodeDrawable(src) { decoder, info, _ ->
            val side = maxOf(info.size.width, info.size.height)
            if (side > MAX_DECODE_SIDE) {
                val scale = MAX_DECODE_SIDE.toFloat() / side
                decoder.setTargetSize(
                    maxOf(1, (info.size.width * scale).toInt()),
                    maxOf(1, (info.size.height * scale).toInt()),
                )
            }
        }
    } else {
        val opts = BitmapFactory.Options().apply { inSampleSize = 1 }
        val bitmap = when (source) {
            is AnimatedSource.OfFile -> BitmapFactory.decodeFile(source.file.absolutePath, opts)
            is AnimatedSource.OfBytes -> BitmapFactory.decodeByteArray(source.bytes, 0, source.bytes.size, opts)
        }
        bitmap?.let { android.graphics.drawable.BitmapDrawable(null, it) }
    }
} catch (e: Exception) {
    Log.w(TAG, "animation did not decode: ${e.javaClass.simpleName}")
    null
}

/**
 * A looping or one-shot video drawn on a `TextureView`, so a Compose `clip`
 * (the video-note circle) applies to it — a `SurfaceView` would ignore the
 * clip. [playing] and [muted] are driven by the caller; [onProgress] reports
 * position / duration while playing; [onCompleted] fires at the end of a
 * non-looping run.
 */
@Composable
internal fun VideoSurface(
    file: File,
    playing: Boolean,
    muted: Boolean,
    looping: Boolean,
    modifier: Modifier = Modifier,
    onProgress: (positionMs: Long, durationMs: Long) -> Unit = { _, _ -> },
    onCompleted: () -> Unit = {},
    onFailed: () -> Unit = {},
) {
    val holder = remember(file) { VideoHolder() }
    val progress by rememberUpdatedState(onProgress)
    val completed by rememberUpdatedState(onCompleted)
    val failed by rememberUpdatedState(onFailed)
    DisposableEffect(file) {
        onDispose { holder.release() }
    }
    LaunchedEffect(holder, playing, muted, looping) {
        holder.apply(playing = playing, muted = muted, looping = looping)
        while (playing) {
            val p = holder.player
            if (p != null && holder.isPrepared) {
                val pos = runCatching { p.currentPosition.toLong() }.getOrDefault(0L)
                val dur = runCatching { p.duration.toLong() }.getOrDefault(0L)
                progress(pos, dur)
            }
            kotlinx.coroutines.delay(100)
        }
    }
    AndroidView(
        factory = { ctx ->
            TextureView(ctx).apply {
                surfaceTextureListener = object : TextureView.SurfaceTextureListener {
                    override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
                        holder.attach(
                            file = file,
                            surface = Surface(st),
                            onCompleted = { completed() },
                            onFailed = { failed() },
                        )
                    }

                    override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) = Unit

                    override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                        holder.release()
                        return true
                    }

                    override fun onSurfaceTextureUpdated(st: SurfaceTexture) = Unit
                }
            }
        },
        modifier = modifier,
    )
}

private class VideoHolder {
    var player: MediaPlayer? = null
    private var surface: Surface? = null
    private var prepared = false
    val isPrepared: Boolean get() = prepared
    private var wantPlaying = false
    private var wantMuted = true
    private var wantLooping = true

    fun attach(file: File, surface: Surface, onCompleted: () -> Unit, onFailed: () -> Unit) {
        release()
        this.surface = surface
        val mp = MediaPlayer()
        player = mp
        try {
            mp.setDataSource(file.absolutePath)
            mp.setSurface(surface)
            mp.setOnPreparedListener {
                prepared = true
                sync()
            }
            mp.setOnCompletionListener { if (!wantLooping) onCompleted() }
            mp.setOnErrorListener { _, _, _ ->
                onFailed()
                true
            }
            mp.prepareAsync()
        } catch (e: Exception) {
            Log.w(TAG, "video did not open: ${e.javaClass.simpleName}")
            onFailed()
            release()
        }
    }

    fun apply(playing: Boolean, muted: Boolean, looping: Boolean) {
        wantPlaying = playing
        wantMuted = muted
        wantLooping = looping
        sync()
    }

    private fun sync() {
        val mp = player ?: return
        if (!prepared) return
        runCatching {
            mp.isLooping = wantLooping
            val v = if (wantMuted) 0f else 1f
            mp.setVolume(v, v)
            if (wantPlaying && !mp.isPlaying) {
                mp.start()
            } else if (!wantPlaying && mp.isPlaying) {
                mp.pause()
            }
        }
    }

    fun release() {
        val mp = player
        player = null
        prepared = false
        if (mp != null) {
            runCatching { mp.stop() }
            runCatching { mp.release() }
        }
        surface?.release()
        surface = null
    }
}

/**
 * The voice waveform as a seekable bar chart. Exposed to TalkBack as a slider
 * (UX spec §7: "the waveform is exposed as a slider for seeking").
 *
 * [onLongPress] is the bubble's action menu, and passing it is what makes the
 * waveform obey `docs/ux/client-redesign.md`'s rule that the deliberate
 * gesture reaches the menu from anywhere on the bubble, INCLUDING over the
 * bubble's own controls, while a press long enough to be that gesture is not
 * also a tap on the control underneath it. Both halves come from handing the
 * callback to `detectTapGestures`, whose long-press branch fires it and then
 * suppresses the tap for that gesture — rather than from letting the press
 * through to the ancestor Surface, which would leave "was that a seek?" to
 * Compose's consumption arbitration between a `Canvas` and a distant parent.
 *
 * The THRESHOLD is deliberately not iOS's 0.5 s even though the RULE is shared:
 * `detectTapGestures` measures against `viewConfiguration.longPressTimeoutMillis`,
 * the platform's own, because a person's press is judged against the long-press
 * timeout their device taught them — it is a system (and accessibility)
 * setting, and a hardcoded constant would make this bubble disagree with every
 * other Android app on the same phone.
 */
@Composable
internal fun WaveformSeekBar(
    waveform: ByteArray?,
    progress: Float,
    playedColor: Color,
    idleColor: Color,
    label: String,
    modifier: Modifier = Modifier,
    bars: Int = 40,
    /** The bubble's action menu, or `null` where no menu is offered — in which
     *  case there is nothing to hand a long press to and the tap detector goes
     *  back to accepting a press of any length, as it always did. */
    onLongPress: (() -> Unit)? = null,
    onSeek: (Float) -> Unit,
) {
    val heights = remember(waveform, bars) { waveformBars(waveform, bars) }
    val seek by rememberUpdatedState(onSeek)
    val longPress by rememberUpdatedState(onLongPress)
    Canvas(
        modifier = modifier
            .semantics {
                contentDescription = label
                progressBarRangeInfo = ProgressBarRangeInfo(progress.coerceIn(0f, 1f), 0f..1f)
                setProgress { target ->
                    seek(target.coerceIn(0f, 1f))
                    true
                }
                // TalkBack reaches the menu by its own long press, which never
                // travels through the pointer pipeline — so declining a touch
                // long press would have taken the menu away from a screen
                // reader if this were not here. The seek action above stays
                // beside it: both are offered, and the user picks.
                if (onLongPress != null) {
                    onLongClick(label = BUBBLE_ACTIONS_LABEL) {
                        longPress?.invoke()
                        true
                    }
                }
            }
            .pointerInput(onLongPress == null) {
                detectTapGestures(
                    onLongPress = if (onLongPress == null) null else ({ longPress?.invoke() }),
                    onTap = { offset ->
                        if (size.width > 0) seek((offset.x / size.width).coerceIn(0f, 1f))
                    },
                )
            }
            .pointerInput(Unit) {
                detectHorizontalDragGestures { change, _ ->
                    if (size.width > 0) seek((change.position.x / size.width).coerceIn(0f, 1f))
                }
            },
    ) {
        val count = heights.size
        if (count == 0) return@Canvas
        val slot = size.width / count
        val barWidth = (slot * 0.6f).coerceAtLeast(1f)
        heights.forEachIndexed { i, h ->
            val barHeight = (size.height * h).coerceAtLeast(2f)
            val x = i * slot + (slot - barWidth) / 2f
            val y = (size.height - barHeight) / 2f
            val played = (i + 0.5f) / count <= progress
            drawRoundRect(
                color = if (played) playedColor else idleColor,
                topLeft = Offset(x, y),
                size = Size(barWidth, barHeight),
                cornerRadius = CornerRadius(barWidth / 2f, barWidth / 2f),
            )
        }
    }
}
