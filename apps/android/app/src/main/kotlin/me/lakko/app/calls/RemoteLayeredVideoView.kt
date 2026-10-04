// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.os.SystemClock
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import kotlinx.coroutines.delay
import org.webrtc.EglBase
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.layerTickMs

/**
 * A remote person's picture in an SFU call (`docs/ux/group-call-grid.md`
 * section 7.5, ADR-0067 step 5).
 *
 * ## One renderer, a sink on each track
 *
 * The tile has ONE `SurfaceViewRenderer`. Each of the person's tracks (the
 * main one, and the light companion when the publisher sends one) carries a
 * small [VideoSink] of this tile's. Every frame a track delivers is reported
 * to the shared rule ([LayeredTileDriver]), and handed on to the renderer only
 * when that track is the one the rule says to show.
 *
 * Why this and not two stacked renderers switched by alpha or z-order, which
 * is how the web client does it: libwebrtc's Android renderer is a
 * `SurfaceView`. A `SurfaceView` has no alpha, two of them over each other
 * need an explicit z-order that is fixed when the surface is created, and one
 * made invisible loses its surface, which is a renderer torn down. Taking the
 * frames of the chosen track into one surface has none of that: the switch is
 * a hard cut between two frames in the same box with the same scaling, no
 * renderer is created or released by it, and a second EGL surface per tile is
 * not spent.
 *
 * Both tracks keep decoding: a native receive stream decodes whether or not
 * its frames are drawn, and both sinks stay attached for as long as the tile
 * is on screen.
 *
 * Without a companion every main frame is drawn, which is the picture the
 * tile had before; the rule still counts how long it stood still.
 *
 * ## Clock and foreground
 *
 * The rule is ticked every `layerTickMs()` so a picture that stopped is
 * noticed without a frame arriving. While the app is not started nothing is
 * presented, so nothing is reported, and on return the tile starts again on
 * the full picture (`resync`).
 *
 * @param mainTrack the person's main video track.
 * @param lightTrack their light companion, or `null` when they send none.
 * @param peerId the SFU peer id, for the `shown` line of the media counters.
 */
@Composable
internal fun RemoteLayeredVideoView(
    mainTrack: VideoTrack,
    lightTrack: VideoTrack?,
    peerId: String,
    eglContext: EglBase.Context,
    modifier: Modifier = Modifier,
    scalingType: RendererCommon.ScalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
) {
    val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
    val currentMain by rememberUpdatedState(mainTrack)
    val currentLight by rememberUpdatedState(lightTrack)
    val currentScaling by rememberUpdatedState(scalingType)

    val tile = remember(peerId) {
        LayeredTile(
            peerId = peerId,
            driver = LayeredTileDriver.forTile(
                clock = SystemClock::elapsedRealtime,
                lightAvailable = false,
                observing = lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED),
            ),
        )
    }
    val tickMs = remember { layerTickMs().toLong().coerceAtLeast(1L) }

    // The clock: notice a stall without a frame arriving, and hand what was
    // counted to the call's ledger.
    LaunchedEffect(tile) {
        while (true) {
            delay(tickMs)
            tile.tick()
        }
    }

    // Foreground: nothing is observed while the app is stopped, and the tile
    // starts again on the full picture when it returns.
    DisposableEffect(tile, lifecycle) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_STOP -> tile.driver.pause()
                Lifecycle.Event.ON_START -> tile.driver.resume()
                else -> Unit
            }
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }

    // Keyed on the tile, so a tile for another peer gets a renderer of its
    // own rather than this one's with its sinks.
    androidx.compose.runtime.key(tile) {
        AndroidView(
            modifier = modifier,
            factory = { ctx ->
                val view = SurfaceViewRenderer(ctx)
                view.init(eglContext, null)
                view.setScalingType(currentScaling)
                // Never mirror the other person.
                view.setMirror(false)
                tile.renderer = view
                tile.attach(currentMain, currentLight)
                view
            },
            update = { view ->
                view.setScalingType(currentScaling)
                tile.attach(currentMain, currentLight)
            },
            onRelease = { view ->
                tile.release()
                view.release()
            },
        )
    }
}

/**
 * The state behind one [RemoteLayeredVideoView]: the two sinks, which tracks
 * they are on, and the driver. Created and released on the main thread; the
 * sinks' `onFrame` runs on libwebrtc's decoder threads.
 */
private class LayeredTile(
    val peerId: String,
    val driver: LayeredTileDriver,
) {
    /** Where drawn frames go. Read on the decoder threads. */
    @Volatile
    var renderer: VideoSink? = null

    private var attachedMain: VideoTrack? = null
    private var attachedLight: VideoTrack? = null
    private var released = false

    private val mainSink = VideoSink { frame: VideoFrame ->
        if (driver.mainFrame()) renderer?.onFrame(frame)
    }

    private val lightSink = VideoSink { frame: VideoFrame ->
        if (driver.lightFrame()) renderer?.onFrame(frame)
    }

    /** Put the sinks on [main] and [light], moving them off whatever they
     *  were on, and tell the rule when the companion appears or goes. */
    fun attach(main: VideoTrack?, light: VideoTrack?) {
        if (released) return
        if (main !== attachedMain) {
            attachedMain?.let { detach(it, mainSink) }
            attachedMain = main?.takeIf { attach(it, mainSink) }
        }
        if (light !== attachedLight) {
            attachedLight?.let { detach(it, lightSink) }
            attachedLight = light?.takeIf { attach(it, lightSink) }
            driver.setLightAvailable(attachedLight != null)
        }
    }

    /** One look at the clock, and what was counted goes to the ledger. */
    fun tick() {
        if (released) return
        driver.tick()
        driver.drain()?.let { shownLayerLedger.add(peerId, it) }
    }

    /** The tile is leaving the screen: take the sinks off, hand over the last
     *  counts, release the rule. */
    fun release() {
        if (released) return
        released = true
        attachedMain?.let { detach(it, mainSink) }
        attachedLight?.let { detach(it, lightSink) }
        attachedMain = null
        attachedLight = null
        renderer = null
        driver.close()?.let { shownLayerLedger.add(peerId, it) }
    }

    private fun attach(track: VideoTrack, sink: VideoSink): Boolean =
        try {
            track.addSink(sink)
            true
        } catch (cause: RuntimeException) {
            // The track was disposed with its connection.
            false
        }

    private fun detach(track: VideoTrack, sink: VideoSink) {
        try {
            track.removeSink(sink)
        } catch (cause: RuntimeException) {
            // The track was disposed with its connection; its sinks went with it.
        }
    }
}
