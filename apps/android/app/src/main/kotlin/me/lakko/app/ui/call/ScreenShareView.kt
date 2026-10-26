// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.graphics.SurfaceTexture
import android.os.Handler
import android.os.Looper
import android.view.TextureView
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ScreenShare
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.ShareZoom
import me.lakko.app.calls.WebRtcPeerConnection
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.ui.theme.LakkoDark
import org.webrtc.EglBase
import org.webrtc.EglRenderer
import org.webrtc.GlRectDrawer
import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import org.webrtc.VideoTrack

/**
 * A shared screen as a receiver shows it (ADR-0083, `docs/ux/screen-share.md`
 * section 5): always whole and uncropped, on black.
 *
 * Drawn on a [TextureView] rather than the `SurfaceViewRenderer` the camera
 * tiles use, because a surface view does not follow the view transform the
 * zoom is (section 5.3): the picture is fitted by sizing the view to the
 * frame's own shape and zoomed by scaling it.
 *
 * @param zoomable the picture is in the large place (the stage, or the
 *   one-to-one's main picture): pinch to zoom, drag to pan, double tap back
 *   to whole. The zoom resets whenever this changes or the track does.
 * @param onTap a single tap, where the caller has a use for one.
 */
@Composable
internal fun ScreenSharePicture(
    track: VideoTrack?,
    sharerName: String,
    zoomable: Boolean,
    modifier: Modifier = Modifier,
    compact: Boolean = false,
    onTap: (() -> Unit)? = null,
) {
    val strings = LocalStrings.current
    val egl = remember { WebRtcPeerConnection.sharedEglContext() }
    // Width over height of the frames as drawn, once the first has arrived.
    var frameAspect by remember(track) { mutableStateOf<Float?>(null) }
    var zoom by remember(track, zoomable) { mutableStateOf(ShareZoom.Whole) }
    val waiting = track == null || egl == null || frameAspect == null
    val label = strings.format(
        if (waiting) "call.screen_share.waiting" else "call.screen_share.tile",
        mapOf("name" to sharerName),
    )
    Box(
        modifier = modifier
            .background(Color.Black)
            .clipToBounds()
            .semantics(mergeDescendants = true) { contentDescription = label }
            .pointerInput(zoomable, onTap) {
                detectTapGestures(
                    onDoubleTap = if (zoomable) {
                        { zoom = ShareZoom.Whole }
                    } else {
                        null
                    },
                    onTap = onTap?.let { tap -> { tap() } },
                )
            }
            .then(
                if (zoomable) {
                    Modifier.pointerInput(Unit) {
                        detectTransformGestures { _, pan, gestureZoom, _ ->
                            zoom = zoom.step(gestureZoom, pan.x, pan.y, size.width.toFloat(), size.height.toFloat())
                        }
                    }
                } else {
                    Modifier
                },
            ),
        contentAlignment = Alignment.Center,
    ) {
        if (track != null && egl != null) {
            val aspect = frameAspect
            ShareTextureView(
                track = track,
                egl = egl,
                onFrameAspect = { frameAspect = it },
                modifier = (if (aspect != null && aspect > 0f) Modifier.aspectRatio(aspect) else Modifier.fillMaxSize())
                    .graphicsLayer {
                        scaleX = zoom.scale
                        scaleY = zoom.scale
                        translationX = zoom.offsetX
                        translationY = zoom.offsetY
                    },
            )
        }
        if (waiting) {
            // Section 5.3: from the announcement to the first frame, the screen
            // glyph, a spinner and the words.
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.padding(12.dp).clearAndSetSemantics { },
            ) {
                Icon(
                    Icons.Filled.ScreenShare,
                    contentDescription = null,
                    tint = LakkoDark.textMuted,
                    modifier = Modifier.size(if (compact) 20.dp else 32.dp),
                )
                CircularProgressIndicator(
                    modifier = Modifier.size(if (compact) 14.dp else 20.dp),
                    color = LakkoDark.textMuted,
                    strokeWidth = 2.dp,
                )
                if (!compact) {
                    Text(
                        text = label,
                        color = LakkoDark.textMuted,
                        fontSize = 13.sp,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/**
 * The share's name strip: a screen glyph where a person's tile has its mic
 * glyph, and `Anna's screen` (`docs/ux/group-call-grid.md` section 6.3).
 */
@Composable
internal fun ScreenShareNameStrip(sharerName: String, short: Boolean, modifier: Modifier = Modifier) {
    val strings = LocalStrings.current
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(LakkoDark.background.copy(alpha = 0.45f))
            .padding(horizontal = if (short) 6.dp else 10.dp, vertical = if (short) 4.dp else 8.dp)
            .clearAndSetSemantics { },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(if (short) 4.dp else 8.dp),
    ) {
        Icon(
            Icons.Filled.ScreenShare,
            contentDescription = null,
            tint = LakkoDark.text,
            modifier = Modifier.size(12.dp),
        )
        Text(
            text = strings.format("call.screen_share.tile", mapOf("name" to sharerName)),
            color = LakkoDark.text,
            fontSize = 12.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

/**
 * The chip on the stage while a pin holds and someone shares (section 5.2):
 * `Anna is sharing their screen` and `View`, which clears the pin so the
 * share takes the stage.
 */
@Composable
internal fun ScreenShareOfferChip(sharerName: String, onView: () -> Unit, modifier: Modifier = Modifier) {
    val strings = LocalStrings.current
    Row(
        modifier = modifier
            .background(LakkoDark.background.copy(alpha = 0.8f), RoundedCornerShape(20.dp))
            .padding(start = 14.dp, end = 4.dp, top = 2.dp, bottom = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Icon(
            Icons.Filled.ScreenShare,
            contentDescription = null,
            tint = LakkoDark.text,
            modifier = Modifier.size(14.dp),
        )
        Text(
            text = strings.format("call.screen_share.pinned_chip", mapOf("name" to sharerName)),
            color = LakkoDark.text,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        TextButton(onClick = onView) {
            Text(strings["call.screen_share.view"], color = LakkoDark.accent, fontSize = 13.sp)
        }
    }
}

/**
 * Reads each share announcement once, politely (section 5.3): a live region
 * that is not drawn. [nameOf] names the sharer of a share by its peer id.
 */
@Composable
internal fun ScreenShareAnnouncer(state: CallUiState, nameOf: (peerId: String) -> String) {
    val event by state.shareAnnouncement.collectAsState()
    val strings = LocalStrings.current
    var spoken by remember { mutableStateOf("") }
    LaunchedEffect(event?.serial) {
        val e = event ?: return@LaunchedEffect
        spoken = strings.format(e.announcement.key, mapOf("name" to nameOf(e.announcement.peerId)))
    }
    Box(
        modifier = Modifier
            .size(1.dp)
            .semantics {
                liveRegion = LiveRegionMode.Polite
                contentDescription = spoken
            },
    )
}

/** A texture view drawing [track] through its own [EglRenderer]. */
@Composable
private fun ShareTextureView(
    track: VideoTrack,
    egl: EglBase.Context,
    onFrameAspect: (Float) -> Unit,
    modifier: Modifier = Modifier,
) {
    val holder = remember { ShareRendererHolder() }
    holder.onFrameAspect = onFrameAspect
    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            TextureView(ctx).also { view ->
                holder.start(view, egl)
                holder.attach(track)
            }
        },
        update = { holder.attach(track) },
        onRelease = { holder.release() },
    )
}

/**
 * The renderer behind [ShareTextureView]: the texture's EGL surface, the
 * track it draws, and the frame shape it reports. Frames arrive on a decoder
 * thread; the shape is reported on the main thread, only when it changes.
 */
private class ShareRendererHolder : VideoSink {
    private val renderer = EglRenderer("lakko-screen-share")
    private val main = Handler(Looper.getMainLooper())
    private var started = false
    private var attached: VideoTrack? = null

    @Volatile
    private var lastAspect: Float = 0f

    @Volatile
    var onFrameAspect: (Float) -> Unit = {}

    fun start(view: TextureView, egl: EglBase.Context) {
        if (started) return
        started = true
        renderer.init(egl, EglBase.CONFIG_PLAIN, GlRectDrawer())
        view.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(surface: SurfaceTexture, width: Int, height: Int) {
                renderer.createEglSurface(surface)
            }

            override fun onSurfaceTextureSizeChanged(surface: SurfaceTexture, width: Int, height: Int) = Unit

            override fun onSurfaceTextureDestroyed(surface: SurfaceTexture): Boolean {
                val done = CountDownLatch(1)
                renderer.releaseEglSurface { done.countDown() }
                done.await(RELEASE_WAIT_MS, TimeUnit.MILLISECONDS)
                return true
            }

            override fun onSurfaceTextureUpdated(surface: SurfaceTexture) = Unit
        }
    }

    fun attach(track: VideoTrack) {
        if (attached === track) return
        attached?.removeSink(this)
        lastAspect = 0f
        track.addSink(this)
        attached = track
    }

    fun release() {
        attached?.removeSink(this)
        attached = null
        if (started) renderer.release()
        started = false
    }

    override fun onFrame(frame: VideoFrame) {
        val h = frame.rotatedHeight
        if (h > 0) {
            val aspect = frame.rotatedWidth.toFloat() / h.toFloat()
            if (aspect != lastAspect) {
                lastAspect = aspect
                main.post { onFrameAspect(aspect) }
            }
        }
        renderer.onFrame(frame)
    }

    private companion object {
        const val RELEASE_WAIT_MS = 500L
    }
}
