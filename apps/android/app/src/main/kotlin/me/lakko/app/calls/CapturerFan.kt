// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.CapturerObserver
import org.webrtc.VideoFrame
import java.util.concurrent.CopyOnWriteArrayList

/**
 * One camera capturer feeding several video sources: what gives each of the
 * group call's camera senders (`CameraSenders.kt`) a source of its own.
 *
 * ## Why not one source for every sender
 *
 * libwebrtc lets an encoder ask its source for fewer pixels (bandwidth, the
 * quality scaler, the frames dropped at a low start bitrate). A source fed to
 * several tracks serves the smallest picture any of its encoders asked for:
 * its broadcaster takes the minimum of every sink's wanted pixel count, and
 * the source's adapter scales the frames before any sender sees them. With
 * the main, middle and light senders on one source, all three encoders were
 * re-initialised to smaller sizes in the first second of the call of
 * 2026-11-07 on the Pixel 7a (start bitrate 100 kbit/s), and the picture
 * never came back: the main sender stayed at 480x270 while it sent 2.5
 * Mbit/s, and the middle sender, which every grid tile watched, at 240x135,
 * half of that rather than half of the 1280x720 capture.
 *
 * The web client sends clones of the camera track, and its tiles looked sharp
 * in the same call. Here each copy gets a `VideoSource` of its own and the capturer's
 * frames are handed to every source in turn, so an encoder narrows only its
 * own source. Each copy's source scales the frames down to the copy's size
 * ([cameraCopySize]); an encoding's `scaleResolutionDownBy` did not reach the
 * frames ([CAMERA_LIGHT_SCALE_DOWN]).
 *
 * ## Threading
 *
 * Frames and capturer state arrive on the capture thread; observers are
 * attached from the call's thread. The list is copy-on-write, and an observer
 * attached while the capturer runs is told it started, as the first one was.
 * A frame is handed to each observer synchronously: libwebrtc's own observer
 * makes its adapted copy before returning, and the capturer releases the
 * frame after this returns.
 *
 * ## The camera background
 *
 * When a [stage] is set (the camera background of ADR-0089), each frame
 * goes through it ONCE, before any observer sees it, so every copy of the
 * camera and the self-view get the same processed frame and no copy can
 * carry the room. A frame the stage drops reaches no observer at all. The
 * Settings sheet's preview ([CameraBackground]) is handed the same frame
 * the observers get.
 */
internal class CapturerFan(
    first: CapturerObserver,
    /** The step every frame passes before the observers; `null` hands the
     *  camera frames on as they are. */
    internal val stage: CameraFrameStage? = null,
    /** Told when the capturer starts and stops (the preview's registry). */
    private val onRunning: (running: Boolean) -> Unit = {},
    /** Handed each frame the observers get (the sheet's preview). */
    private val preview: (VideoFrame) -> Unit = {},
) : CapturerObserver {
    private val observers = CopyOnWriteArrayList<CapturerObserver>(listOf(first))

    /** What the capturer last said of its start: `null` before it started
     *  and after it stopped. */
    @Volatile
    private var started: Boolean? = null

    /** The number of observers frames are handed to. */
    val size: Int
        get() = observers.size

    /** Hand frames to [observer] too. Attaching one twice does nothing. */
    fun attach(observer: CapturerObserver) {
        if (!observers.addIfAbsent(observer)) return
        started?.let { observer.onCapturerStarted(it) }
    }

    /** Stop handing frames to [observer]. The first one is never detached. */
    fun detach(observer: CapturerObserver) {
        if (observers.firstOrNull() === observer) return
        observers.remove(observer)
    }

    override fun onCapturerStarted(success: Boolean) {
        started = success
        if (success) stage?.onCapturerStarted()
        onRunning(success)
        for (observer in observers) observer.onCapturerStarted(success)
    }

    override fun onCapturerStopped() {
        started = null
        onRunning(false)
        for (observer in observers) observer.onCapturerStopped()
    }

    override fun onFrameCaptured(frame: VideoFrame) {
        val out = if (stage == null) frame else (stage.process(frame) ?: return)
        try {
            for (observer in observers) observer.onFrameCaptured(out)
            preview(out)
        } finally {
            if (out !== frame) out.release()
        }
    }
}
