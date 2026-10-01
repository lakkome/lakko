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
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
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
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerEventTimeoutCancellationException
import androidx.compose.ui.input.pointer.PointerId
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.changedToUpIgnoreConsumed
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
 * also a tap on the control underneath it — and its companion, that once the
 * menu has opened the control underneath it receives nothing more from that
 * press, not a tap on release and not a drag. Both come from
 * [trackWaveformPress], which classifies the press once; see its own
 * documentation for why one detector rather than two.
 *
 * The THRESHOLD is deliberately not iOS's 0.5 s even though the RULE is shared:
 * the classification measures against
 * `viewConfiguration.longPressTimeoutMillis`, the platform's own, because a
 * person's press is judged against the long-press timeout their device taught
 * them — it is a system (and accessibility) setting, and a hardcoded constant
 * would make this bubble disagree with every other Android app on the same
 * phone.
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
                    onLongClick(label = me.lakko.app.l10n.Localization.strings.value[BUBBLE_ACTIONS_LABEL]) {
                        longPress?.invoke()
                        true
                    }
                }
            }
            .pointerInput(onLongPress == null) {
                awaitEachGesture { trackWaveformPress(longPress, seek) }
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

/**
 * One press over the waveform, classified once as a long press, a scrub or a
 * tap, and answered as exactly one of the three.
 *
 * **Why one detector and not two.** This used to be a `detectTapGestures` for
 * the tap and the long press beside a `detectHorizontalDragGestures` for the
 * scrub, and that arrangement could not express the rule at all: a long press
 * and a scrub are two outcomes of the SAME press, and two detectors that never
 * speak to each other cannot agree on which of them happened. They did not.
 * A press held until the menu opened and then slid sideways fired the menu from
 * the first detector and, because the drag detector is the inner node and sees
 * a move on the main pass before the outer one consumes it, seeked from the
 * second — so one press opened the menu AND moved the playhead under it, which
 * `docs/ux/client-redesign.md` names as the thing that must not happen.
 *
 * A shared "the menu already opened" flag between the two detectors would have
 * made the common case work and left the guarantee resting on which of two
 * coroutines resumes first when the long-press timeout and a move land in the
 * same instant. The rule is about what the control does, so it belongs in one
 * state machine that decides once: past this classification there is no code
 * path on which a long press also seeks.
 *
 * The three outcomes, whichever comes first after the finger goes down:
 *  - the platform's long-press timeout expires -> [onLongPress], and the press
 *    is over as far as this control is concerned. What remains of it is
 *    consumed so no ancestor acts on it either, and nothing seeks.
 *  - the finger travels past touch slop -> a scrub, which seeks to each new
 *    position until the finger lifts.
 *  - the finger lifts -> a tap, which seeks to where it lifted.
 *
 * With no [onLongPress] there is no menu to open, so the timeout is not run and
 * the press is a tap or a scrub as it always was.
 */
private suspend fun AwaitPointerEventScope.trackWaveformPress(
    onLongPress: (() -> Unit)?,
    onSeek: (Float) -> Unit,
) {
    val down = awaitFirstDown(requireUnconsumed = false)
    // Consumed for the same reason `detectTapGestures` consumes it: the press
    // belongs to this control until the classification says otherwise.
    down.consume()

    val width = size.width
    val slop = viewConfiguration.touchSlop
    var position = down.position
    var travelled = 0f

    fun seekTo(x: Float) {
        if (width > 0) onSeek((x / width).coerceIn(0f, 1f))
    }

    if (onLongPress != null) {
        var lifted = false
        var taken = false
        val heldLongEnough = try {
            withTimeout(viewConfiguration.longPressTimeoutMillis) {
                while (travelled <= slop && !lifted && !taken) {
                    val change = currentChange(down.id)
                    if (change == null || change.isConsumed) {
                        taken = true
                    } else {
                        travelled += (change.position - change.previousPosition).getDistance()
                        position = change.position
                        lifted = change.changedToUp()
                        change.consume()
                    }
                }
            }
            false
        } catch (_: PointerEventTimeoutCancellationException) {
            true
        }
        if (heldLongEnough) {
            onLongPress()
            consumeThroughLift(down.id)
            return
        }
        if (taken) return
        if (lifted) {
            if (travelled <= slop) seekTo(position.x)
            return
        }
        // Fell out of the loop past the slop with the finger still down: a
        // scrub, and its first seek is owed here because the move that crossed
        // the slop has already been read.
        seekTo(position.x)
    }

    var scrubbing = travelled > slop
    while (true) {
        val change = currentChange(down.id) ?: return
        if (change.isConsumed) return
        if (change.changedToUp()) {
            change.consume()
            if (!scrubbing) seekTo(change.position.x)
            return
        }
        travelled += (change.position - change.previousPosition).getDistance()
        if (travelled > slop) scrubbing = true
        if (scrubbing) {
            change.consume()
            seekTo(change.position.x)
        }
    }
}

/** This gesture's pointer in the next event, or `null` once it is gone. */
private suspend fun AwaitPointerEventScope.currentChange(id: PointerId): PointerInputChange? =
    awaitPointerEvent().changes.firstOrNull { it.id == id }

/**
 * Swallow what is left of a press whose outcome has already been delivered, so
 * neither this control nor any ancestor acts on it a second time.
 */
private suspend fun AwaitPointerEventScope.consumeThroughLift(id: PointerId) {
    while (true) {
        val change = currentChange(id) ?: return
        change.consume()
        if (change.changedToUpIgnoreConsumed()) return
    }
}
