// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.view.PreviewView
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Cameraswitch
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import me.lakko.app.media.CaptureKind
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.FinishDisposition
import me.lakko.app.media.MIN_TAKE_MS
import me.lakko.app.media.PermissionHint
import me.lakko.app.media.PointerKind
import me.lakko.app.media.RecEffect
import me.lakko.app.media.RecEvent
import me.lakko.app.media.RecHint
import me.lakko.app.media.RecState
import me.lakko.app.media.RecordedTake
import me.lakko.app.media.RecorderThread
import me.lakko.app.media.SILENCE_WARNING_MS
import me.lakko.app.media.SentTakeOutcome
import me.lakko.app.media.SilenceWatch
import me.lakko.app.media.TAP_MAX_MS
import me.lakko.app.media.sentTakeOutcome
import me.lakko.app.media.CameraLens
import me.lakko.app.media.VideoNoteOutcome
import me.lakko.app.media.VideoNoteRecorder
import me.lakko.app.media.VideoStartFailure
import me.lakko.app.media.videoNoteFinishOutcome
import me.lakko.app.media.VoiceRecorder
import me.lakko.app.media.capFor
import me.lakko.app.media.formatClock
import me.lakko.app.media.formatTotalClock
import me.lakko.app.media.isCapturing
import me.lakko.app.media.observeLevel
import me.lakko.app.media.reduce
import me.lakko.app.media.spokenDuration
import java.util.UUID

private const val TAG = "LakkoRecordController"
private const val TICK_MS = 100L
private const val NOTICE_MS = 2_500L

/** A failure hint stays up longer: it explains why a take that looked like it
 *  was recording produced nothing. */
private const val FAILURE_NOTICE_MS = 5_000L

/** UX spec §2 / §3 / §6 copy. */
internal object RecordCopy {
    const val HOLD_TO_RECORD = "Hold to record"
    const val CANCELLED = "Cancelled"
    const val NO_CAMERA = "No camera found"
    const val NO_COMPATIBLE_FORMAT = "Video messages can't be recorded on this device"
    const val START_FAILED = "Couldn't start recording"
    const val FLIP_FAILED = "Couldn't switch camera"
    const val STARTING_CAMERA = "Starting camera"
    const val IN_CALL = "Unavailable during a call"
    const val SLIDE_TO_CANCEL = "Slide to cancel"

    /** §2 "Hearing nothing" — replaces the hint while the input reads zero. */
    const val NO_MIC_SOUND = "No sound from the microphone"

    const val DENIED_MIC = "Lakko needs microphone access to record voice messages"
    const val DENIED_CAMERA = "Lakko needs camera access to record video messages"

    /** §3.1 — the mode-switch accessibility action, named for where it goes. */
    const val SWITCH_TO_VIDEO = "Switch to video message"
    const val SWITCH_TO_VOICE = "Switch to voice message"
}

/**
 * The glue around [reduce]: translates the record button's pointer input, the
 * accessibility activation, the tick and the recorder callbacks into
 * [RecEvent]s, and performs the [RecEffect]s the reducer returns. It decides
 * nothing on its own — every "does this send / lock / cancel / discard" answer
 * comes from the reducer, which is where the tests are.
 *
 * Owns at most one recorder at a time and releases it on [dispose].
 */
@Stable
internal class RecordController(
    private val context: Context,
    private val lifecycleOwner: LifecycleOwner,
    private val hub: ChatMediaHub,
    private val scope: CoroutineScope,
    private val onSend: (RecordedTake) -> Unit,
) {
    var state by mutableStateOf<RecState>(RecState.Idle)
        private set
    var elapsedMs by mutableLongStateOf(0L)
        private set
    var level by mutableFloatStateOf(0f)
        private set

    /**
     * §2 "Hearing nothing" — the microphone has given nothing for
     * [SILENCE_WARNING_MS], so the bar says so in place of its hint. Decided by
     * [observeLevel] over the samples the live meter is already taking; nothing
     * about the take changes because of it.
     */
    var micSilent by mutableStateOf(false)
        private set
    var notice by mutableStateOf<String?>(null)
        private set
    var denied by mutableStateOf<CaptureKind?>(null)
        private set
    var preview by mutableStateOf<RecordedTake?>(null)
        private set
    var mode by mutableStateOf(hub.prefs.recordMode)
        private set
    /** `true` while a video-note take is actually recording (the camera is
     *  bound and CameraX accepted the recording); `false` while it starts. */
    var videoLive by mutableStateOf(false)
        private set

    /** A locked video note can switch to another built-in camera. */
    var canFlip by mutableStateOf(false)
        private set

    private var inCallState by mutableStateOf(false)

    /**
     * `true` while a call owns the microphone and camera (§6). A call that
     * starts while a take is live ends the take — the call owns the audio
     * session and the camera, and a half-recorded take is not sent.
     */
    var inCall: Boolean
        get() = inCallState
        set(value) {
            if (value == inCallState) return
            inCallState = value
            if (value && state !is RecState.Idle) {
                dispatch(if (state is RecState.Preview) RecEvent.Delete else RecEvent.Cancel)
            }
        }

    /** The package manager's view; CameraX may still report fewer cameras. */
    private val cameraFeature: Boolean = VideoNoteRecorder.hasAnyCamera(context)

    /** What CameraX reported, once asked: `null` while unknown. */
    private var cameraLenses: Set<CameraLens>? = null
    private var cameraProbe: Job? = null
    private val cameraPresent: Boolean
        get() = cameraFeature && cameraLenses?.isNotEmpty() != false
    internal var requestPermissions: ((Array<String>) -> Unit)? = null

    /** §2 — the run of silence in the take being recorded now. */
    private var silence = SilenceWatch()

    private var voice: VoiceRecorder? = null
    /** The voice recorder's start, running on [RecorderThread]; `true` once it
     *  is recording. Everything that touches [voice] afterwards awaits it, so
     *  a release that lands while start is still blocked is applied after. */
    private var voiceStart: Deferred<Boolean>? = null
    private var video: VideoNoteRecorder? = null
    private var previewView: PreviewView? = null
    private var tickJob: Job? = null
    private var noticeJob: Job? = null

    /**
     * §3.1 — the arming timer. Started by [press]; if it fires with the pointer
     * still down the press was a hold and the reducer may open the microphone.
     * Cancelled by [dispatch] the moment the state leaves [RecState.Pressing],
     * which is the one place that cannot be forgotten.
     */
    private var holdJob: Job? = null
    private var startJob: Job? = null
    private var captureStartedAt: Long = 0L
    /** When the video-note start began, for [videoNoteFinishOutcome]. */
    private var videoStartRequestedAt: Long = 0L
    private var videoStartFailure: VideoStartFailure? = null

    init {
        if (mode == CaptureKind.VIDEO_NOTE && !cameraPresent) mode = CaptureKind.VOICE
        if (mode == CaptureKind.VIDEO_NOTE) probeCameras()
    }

    /**
     * §3 "No camera present" — ask CameraX which cameras it can bind, so a
     * device where it lists none leaves camera mode before a hold rather than
     * showing a recording UI with no camera behind it. Also warms the CameraX
     * provider, so the first hold binds sooner.
     */
    private fun probeCameras() {
        if (cameraLenses != null || cameraProbe?.isActive == true || !cameraFeature) return
        cameraProbe = scope.launch {
            val provider = VideoNoteRecorder.obtainProvider(context) ?: return@launch
            val lenses = VideoNoteRecorder.cameraInfos(provider).keys
            cameraLenses = lenses
            if (lenses.isEmpty() && mode == CaptureKind.VIDEO_NOTE && state is RecState.Idle) {
                Log.w(TAG, "no camera reported; leaving camera mode")
                mode = CaptureKind.VOICE
                hub.prefs.recordMode = mode
            }
        }
    }

    private fun now(): Long = SystemClock.elapsedRealtime()

    /** Why the record button is disabled, or `null` when it is usable. */
    val disabledReason: String? get() = if (inCall) RecordCopy.IN_CALL else null

    /**
     * §3.1 — the pointer went down. Nothing is captured and no permission is
     * asked for yet: the press is a tap until the arming timer says otherwise.
     */
    fun press(pointer: PointerKind, xDp: Float, yDp: Float) {
        if (disabledReason != null) return
        if (refuseVideoWithoutCamera()) return
        val kind = mode
        dispatch(RecEvent.Press(kind, pointer, xDp, yDp, now()))
        if (state !is RecState.Pressing) return
        holdJob = scope.launch {
            delay(TAP_MAX_MS)
            dispatch(RecEvent.HoldElapsed(now(), permissionHint(kind)))
        }
    }

    /** The label of the §3.1 mode-switch accessibility action. */
    val modeSwitchLabel: String
        get() = if (mode == CaptureKind.VOICE) {
            RecordCopy.SWITCH_TO_VIDEO
        } else {
            RecordCopy.SWITCH_TO_VOICE
        }

    /**
     * §3.1 — the mode switch a screen reader or a switch device reaches, since
     * the tap that carries it for everyone else is a gesture they do not have.
     * Refused while a take is live or a call holds capture, so it can never
     * change the mode out from under a running recording.
     */
    fun switchMode() {
        if (disabledReason != null || state !is RecState.Idle) return
        toggleMode()
    }

    /** Camera mode on a device known to have no camera: say so, record nothing. */
    private fun refuseVideoWithoutCamera(): Boolean {
        if (mode != CaptureKind.VIDEO_NOTE || state !is RecState.Idle || cameraPresent) return false
        mode = CaptureKind.VOICE
        hub.prefs.recordMode = mode
        showFailure(RecordCopy.NO_CAMERA)
        return true
    }

    fun move(xDp: Float, yDp: Float) = dispatch(RecEvent.Move(xDp, yDp))

    fun release() = dispatch(RecEvent.Release(now()))

    /** The system took the gesture away mid-press or mid-hold. */
    fun gestureCancelled() {
        if (state is RecState.Pressing ||
            state is RecState.Recording ||
            state is RecState.Requesting
        ) {
            dispatch(RecEvent.Cancel)
        }
    }

    /** TalkBack / switch access: a single activation starts a LOCKED take. */
    fun activate() {
        if (disabledReason != null) return
        if (refuseVideoWithoutCamera()) return
        dispatch(RecEvent.Activate(mode, now(), permissionHint(mode)))
    }

    fun stop() = dispatch(RecEvent.Stop(now()))

    fun delete() = dispatch(RecEvent.Delete)

    fun cancel() = dispatch(RecEvent.Cancel)

    fun sendPreview() = dispatch(RecEvent.Send)

    fun dismissDenied() {
        denied = null
    }

    fun flipCamera() {
        val v = video ?: return
        if (!v.flip()) showNotice(RecordCopy.FLIP_FAILED)
        canFlip = v.canFlip
    }

    /** The round preview's view came up; this take binds to THIS one. */
    fun attachPreview(view: PreviewView) {
        previewView = view
    }

    /**
     * That view went away. Identity-checked on purpose: the previous take's
     * preview is torn down by Compose, and a blanket `attachPreview(null)`
     * landing after a new take has already attached its own view would blank
     * the view that take is waiting for — and the wait ends in
     * `Couldn't start recording`.
     */
    fun detachPreview(view: PreviewView) {
        if (previewView === view) previewView = null
    }

    fun onPermissionResult() {
        val granted = requiredPermissions(currentKind() ?: mode).all(::isGranted)
        dispatch(RecEvent.PermissionResult(granted, now()))
    }

    private fun currentKind(): CaptureKind? = when (val s = state) {
        // A press still being judged has asked for nothing, so there is no kind
        // in flight; the caller falls back to the current [mode].
        is RecState.Pressing -> null
        is RecState.Requesting -> s.kind
        is RecState.Recording -> s.kind
        is RecState.Locked -> s.kind
        is RecState.Finalizing -> s.kind
        is RecState.Preview -> s.kind
        RecState.Idle -> null
    }

    private fun requiredPermissions(kind: CaptureKind): List<String> =
        if (kind == CaptureKind.VOICE) {
            listOf(Manifest.permission.RECORD_AUDIO)
        } else {
            listOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
        }

    private fun isGranted(permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    /**
     * On Android a request for an ungranted runtime permission always shows the
     * system dialog (or returns at once when permanently denied), so anything
     * short of granted is a PROMPT: the hold that raised it does not record
     * (§6), and a refusal lands on the denied card via the result.
     */
    private fun permissionHint(kind: CaptureKind): PermissionHint =
        if (requiredPermissions(kind).all(::isGranted)) PermissionHint.GRANTED else PermissionHint.PROMPT

    fun dispatch(event: RecEvent) {
        val step = reduce(state, event)
        state = step.state
        // §3.1 — the press has been judged (or never was one): the arming timer
        // must not fire into a state that did not ask for it.
        if (state !is RecState.Pressing) {
            holdJob?.cancel()
            holdJob = null
        }
        for (effect in step.effects) perform(effect)
        if (!isCapturing(state)) {
            tickJob?.cancel()
            tickJob = null
        }
    }

    private fun perform(effect: RecEffect) {
        when (effect) {
            is RecEffect.RequestPermission -> {
                val launcher = requestPermissions
                if (launcher == null) {
                    dispatch(RecEvent.PermissionResult(false, now()))
                } else {
                    launcher(requiredPermissions(effect.kind).filterNot(::isGranted).toTypedArray())
                }
            }
            is RecEffect.StartCapture -> startCapture(effect.kind)
            RecEffect.ReleaseStream -> Unit
            is RecEffect.Finish -> finish(effect.disposition)
            is RecEffect.Hint -> showNotice(
                if (effect.hint == RecHint.HOLD_TO_RECORD) RecordCopy.HOLD_TO_RECORD else RecordCopy.CANCELLED,
            )
            is RecEffect.ShowDenied -> denied = effect.kind
            RecEffect.ToggleMode -> toggleMode()
            RecEffect.SendPreview -> {
                val take = preview
                preview = null
                if (take != null) onSend(take)
            }
            RecEffect.DiscardPreview -> {
                preview?.file?.delete()
                preview = null
            }
        }
    }

    private fun toggleMode() {
        if (mode == CaptureKind.VOICE) {
            if (!cameraPresent) {
                showFailure(RecordCopy.NO_CAMERA)
                return
            }
            mode = CaptureKind.VIDEO_NOTE
            probeCameras()
        } else {
            mode = CaptureKind.VOICE
        }
        hub.prefs.recordMode = mode
    }

    private fun showNotice(text: String, durationMs: Long = NOTICE_MS) {
        notice = text
        noticeJob?.cancel()
        noticeJob = scope.launch {
            delay(durationMs)
            notice = null
        }
    }

    private fun showFailure(text: String) = showNotice(text, FAILURE_NOTICE_MS)

    private fun videoFailureCopy(failure: VideoStartFailure): String = when (failure) {
        VideoStartFailure.NO_CAMERA -> RecordCopy.NO_CAMERA
        VideoStartFailure.NO_COMPATIBLE_FORMAT -> RecordCopy.NO_COMPATIBLE_FORMAT
        VideoStartFailure.FAILED -> RecordCopy.START_FAILED
    }

    private fun startCapture(kind: CaptureKind) {
        elapsedMs = 0L
        level = 0f
        // Per take: the previous take's silence says nothing about this one, and
        // a warning left standing from it would be about a microphone that is
        // no longer being asked anything.
        silence = SilenceWatch()
        micSilent = false
        captureStartedAt = now()
        val stamp = System.currentTimeMillis()
        if (kind == CaptureKind.VOICE) {
            val file = hub.newEntryFile("take-${UUID.randomUUID()}", "voice-$stamp.m4a")
            if (file == null) {
                showNotice(RecordCopy.START_FAILED)
                dispatch(RecEvent.CaptureFailed)
                return
            }
            val rec = VoiceRecorder(context)
            voice = rec
            // prepare() + start() block on the media server: never on main.
            val started = RecorderThread.scope.async {
                rec.start(file) {
                    scope.launch { if (voice === rec) dispatch(RecEvent.CaptureFailed) }
                }
            }
            voiceStart = started
            startTick()
            startJob = scope.launch {
                val ok = started.await()
                // Released, cancelled or disposed meanwhile: finish() owns the
                // recorder now and applies the outcome after this start.
                if (voice !== rec) return@launch
                if (ok) {
                    captureStartedAt = now()
                } else {
                    showNotice(RecordCopy.START_FAILED)
                    dispatch(RecEvent.CaptureFailed)
                }
            }
        } else {
            // Whatever a previous take left holding the camera lets go BEFORE
            // a new one asks for it. Every exit path already finishes its take,
            // so this should be null — but "should be" is what made the second
            // video note fail, and a recorder that is already released treats
            // this as a no-op.
            video?.release()
            // This runs from [dispatch], so the round preview has not
            // recomposed yet: anything still here is the PREVIOUS take's view,
            // whose surface is gone. Binding to it gives a bound camera with a
            // dead preview. Wait for this take's own.
            previewView = null
            val rec = VideoNoteRecorder(context, lifecycleOwner)
            video = rec
            videoLive = false
            canFlip = false
            videoStartFailure = null
            videoStartRequestedAt = now()
            startTick()
            startJob = scope.launch {
                // The round preview composes once the state is Recording /
                // Locked; wait (briefly) for its PreviewView.
                val view = withTimeoutOrNull(PREVIEW_WAIT_MS) {
                    while (previewView == null && isActive) delay(20)
                    previewView
                }
                if (!isCapturing(state) || video !== rec) return@launch
                val bound = if (view == null) {
                    VideoNoteRecorder.BindResult.FAILED
                } else {
                    rec.bind(view)
                }
                if (!isCapturing(state) || video !== rec) return@launch
                if (bound == VideoNoteRecorder.BindResult.BOUND) {
                    cameraLenses = rec.availableLenses
                } else if (bound == VideoNoteRecorder.BindResult.NO_CAMERA) {
                    cameraLenses = emptySet()
                }
                val failure = when (bound) {
                    VideoNoteRecorder.BindResult.NO_CAMERA -> VideoStartFailure.NO_CAMERA
                    VideoNoteRecorder.BindResult.NO_COMPATIBLE_FORMAT ->
                        VideoStartFailure.NO_COMPATIBLE_FORMAT
                    VideoNoteRecorder.BindResult.FAILED -> VideoStartFailure.FAILED
                    VideoNoteRecorder.BindResult.BOUND -> {
                        val file = hub.newEntryFile("take-${UUID.randomUUID()}", "video-note-$stamp.mp4")
                        val started = file != null && rec.start(file) {
                            scope.launch {
                                if (video !== rec) return@launch
                                Log.w(TAG, "video note recording failed")
                                showFailure(RecordCopy.START_FAILED)
                                dispatch(RecEvent.CaptureFailed)
                            }
                        }
                        if (started) null else VideoStartFailure.FAILED
                    }
                }
                if (failure != null) {
                    // Never leave a recording UI up with no camera behind it:
                    // say why, release the camera, return to the composer.
                    Log.w(TAG, "video note capture did not start: $failure")
                    videoStartFailure = failure
                    showFailure(videoFailureCopy(failure))
                    dispatch(RecEvent.CaptureFailed)
                } else {
                    captureStartedAt = now()
                    videoLive = true
                    canFlip = rec.canFlip
                }
            }
        }
    }

    /**
     * One level reading from the live recorder, taken at [atMs] of the take's
     * elapsed time: it drives the meter and, through [observeLevel], §2's
     * "Hearing nothing" warning.
     *
     * `internal` rather than private because it is also where
     * `SilentMicrophoneUiTest` hands in a sample: Robolectric's `MediaRecorder`
     * reports amplitude 0 and cannot be made to hear anything, so a JVM test can
     * reach the silent case for free but needs this to reach the other one. The
     * fold, the state and the bar on the far side of it are the production ones.
     */
    internal fun onLevelSample(level: Double, atMs: Long) {
        this.level = level.toFloat()
        silence = observeLevel(silence, level, atMs)
        micSilent = silence.warns
    }

    private fun startTick() {
        tickJob?.cancel()
        tickJob = scope.launch {
            while (isActive) {
                delay(TICK_MS)
                val kind = currentKind() ?: break
                val v = voice
                val starting = (v != null && voiceStart?.isCompleted != true) ||
                    (video != null && !videoLive)
                elapsedMs = if (starting) 0L else now() - captureStartedAt
                if (v != null && !starting) {
                    onLevelSample(
                        withContext(RecorderThread.dispatcher) { v.sampleLevel() },
                        elapsedMs,
                    )
                }
                if (!requiredPermissions(kind).all(::isGranted)) {
                    dispatch(RecEvent.Revoked)
                    break
                }
                dispatch(RecEvent.Tick(now()))
            }
        }
    }

    private fun finish(disposition: FinishDisposition) {
        tickJob?.cancel()
        tickJob = null
        startJob?.cancel()
        startJob = null
        val v = voice
        val vStart = voiceStart
        val vn = video
        val videoWasLive = videoLive
        val videoFailure = videoStartFailure
        voice = null
        voiceStart = null
        video = null
        videoLive = false
        canFlip = false
        videoStartFailure = null
        val elapsed = now() - captureStartedAt
        val sinceVideoStart = now() - videoStartRequestedAt
        when (disposition) {
            FinishDisposition.DISCARD -> {
                if (v != null) {
                    RecorderThread.scope.launch {
                        vStart?.let { runCatching { it.await() } }
                        v.discard()
                    }
                }
                vn?.release()
            }
            FinishDisposition.SEND, FinishDisposition.PREVIEW -> {
                // The voice stop runs in the process-wide recorder scope, not in
                // this screen's: if the screen goes away while it waits for a
                // slow start, the microphone is still stopped and released.
                // It waits for a start still in progress, so a release during a
                // slow start never leaves a recording running.
                val voiceTake: Deferred<VoiceFinish>? = v?.let { rec ->
                    RecorderThread.scope.async {
                        val ok = vStart?.let { runCatching { it.await() }.getOrDefault(false) } ?: true
                        VoiceFinish(started = ok, take = if (ok) rec.finish() else null)
                    }
                }
                scope.launch {
                    var captureStarted = false
                    val take = try {
                        when {
                            voiceTake != null -> voiceTake.await().also { captureStarted = it.started }.take
                            // `finish` lets the camera go the moment the
                            // recording is finalized; this catches the paths
                            // that never got there (no recording, cancelled
                            // mid-await) and is inert otherwise.
                            vn != null -> try {
                                vn.finish(elapsed)
                            } finally {
                                vn.release()
                            }
                            else -> null
                        }
                    } catch (e: CancellationException) {
                        // Screen disposed mid-finish: nothing will send or preview
                        // this take, so its file must not linger.
                        voiceTake?.let { d ->
                            RecorderThread.scope.launch { d.await().take?.file?.delete() }
                        }
                        throw e
                    }
                    if (vn != null) {
                        val outcome = videoNoteFinishOutcome(
                            takeDurationMs = take?.durationMs,
                            startFailure = videoFailure,
                            recording = videoWasLive,
                            recordedMs = elapsed,
                            sinceStartMs = sinceVideoStart,
                        )
                        if (outcome != VideoNoteOutcome.USABLE) {
                            Log.w(TAG, "video note take ended without a clip: $outcome")
                            deleteOffMain(take?.file)
                            when (outcome) {
                                VideoNoteOutcome.HOLD_TO_RECORD -> showNotice(RecordCopy.HOLD_TO_RECORD)
                                VideoNoteOutcome.NO_CAMERA -> showFailure(RecordCopy.NO_CAMERA)
                                VideoNoteOutcome.NO_COMPATIBLE_FORMAT ->
                                    showFailure(RecordCopy.NO_COMPATIBLE_FORMAT)
                                else -> showFailure(RecordCopy.START_FAILED)
                            }
                            if (disposition == FinishDisposition.PREVIEW) dispatch(RecEvent.PreviewFailed)
                            return@launch
                        }
                    }
                    if (disposition == FinishDisposition.PREVIEW) {
                        if (take == null) {
                            dispatch(RecEvent.PreviewFailed)
                        } else {
                            preview = take
                            dispatch(RecEvent.PreviewReady(take.durationMs))
                        }
                        return@launch
                    }
                    when (sentTakeOutcome(take?.durationMs, captureStarted)) {
                        SentTakeOutcome.SEND -> if (take != null) onSend(take)
                        SentTakeOutcome.HOLD_TO_RECORD -> {
                            deleteOffMain(take?.file)
                            showNotice(RecordCopy.HOLD_TO_RECORD)
                        }
                        SentTakeOutcome.START_FAILED -> {
                            deleteOffMain(take?.file)
                            showNotice(RecordCopy.START_FAILED)
                        }
                    }
                }
            }
        }
    }

    /** What the recorder thread hands back for a voice take. */
    private class VoiceFinish(val started: Boolean, val take: RecordedTake?)

    private fun deleteOffMain(file: java.io.File?) {
        if (file != null) RecorderThread.scope.launch { file.delete() }
    }

    fun dispose() {
        tickJob?.cancel()
        startJob?.cancel()
        noticeJob?.cancel()
        holdJob?.cancel()
        voice?.let { v ->
            val s = voiceStart
            RecorderThread.scope.launch {
                s?.let { runCatching { it.await() } }
                v.discard()
            }
        }
        voiceStart = null
        video?.release()
        voice = null
        video = null
        videoLive = false
        cameraProbe?.cancel()
        preview?.file?.delete()
        preview = null
        state = RecState.Idle
    }

    private companion object {
        const val PREVIEW_WAIT_MS = 2_000L
    }
}

@Composable
internal fun rememberRecordController(
    hub: ChatMediaHub,
    onSend: (RecordedTake) -> Unit,
): RecordController {
    val context = LocalContext.current
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val controller = remember(hub) {
        RecordController(context, lifecycleOwner, hub, scope, onSend)
    }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { controller.onPermissionResult() }
    controller.requestPermissions = { perms -> launcher.launch(perms) }
    DisposableEffect(controller) {
        onDispose { controller.dispose() }
    }
    return controller
}

/**
 * The composer's right-hand record button: ONE button (§3.1). A tap switches
 * between mic and camera and records nothing; a hold records, with slide left
 * to cancel and slide up to lock.
 *
 * Both gestures reach a screen reader as the two actions §3.1 requires: the
 * default activation starts a LOCKED take (§7, since hold-and-slide is not
 * reachable), and a custom action switches the mode — without it the camera
 * would be unreachable, because the gesture that switches is the one a
 * TalkBack user does not have.
 *
 * Coordinates are fed to the reducer in dp, so its slide thresholds mean the
 * same physical distance on every screen density.
 */
@Composable
internal fun RecordButton(controller: RecordController, modifier: Modifier = Modifier) {
    val density = LocalDensity.current.density
    val haptics = LocalHapticFeedback.current
    val disabled = controller.disabledReason
    val kind = controller.mode
    val label = if (kind == CaptureKind.VOICE) "Record voice message" else "Record video message"
    val holding = controller.state is RecState.Recording
    LaunchedEffect(holding) {
        if (holding) haptics.performHapticFeedback(HapticFeedbackType.LongPress)
    }
    // §3.1 — a light tick when the mode switches. A tap that only changes a
    // glyph and is otherwise silent reads as a dead button; recording start
    // already has its own, heavier [HapticFeedbackType.LongPress] above.
    // `TextHandleMove` is the lighter of the exactly two types this Compose
    // version offers, so it is the distinct tick the spec asks for rather than
    // a preference among many.
    //
    // Seeded from the current mode and compared rather than keyed on `kind`
    // alone, so entering the chat — or a configuration change, which rebuilds
    // the composable over a `remember`ed controller that kept its mode — is
    // not mistaken for a switch.
    var tickedMode by remember { mutableStateOf(kind) }
    LaunchedEffect(kind) {
        if (kind != tickedMode) {
            tickedMode = kind
            haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        }
    }
    Box(contentAlignment = Alignment.Center, modifier = modifier.size(48.dp)) {
    if (holding) {
        // §2.5 — the lock target the finger slides up onto. A sibling of the
        // clipped button, so it is drawn above it rather than clipped away.
        Icon(
            imageVector = Icons.Filled.Lock,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.offset { IntOffset(0, (-64 * density).toInt()) },
        )
    }
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(48.dp)
            .clip(CircleShape)
            .background(
                if (holding) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
            )
            .alpha(if (disabled != null) 0.4f else 1f)
            .semantics {
                contentDescription = label
                if (disabled != null) {
                    disabled()
                    stateDescription = disabled
                } else {
                    onClick(label = label) {
                        controller.activate()
                        true
                    }
                    // §3.1 — the second action, so the mode is reachable
                    // without the tap gesture.
                    customActions = listOf(
                        CustomAccessibilityAction(controller.modeSwitchLabel) {
                            controller.switchMode()
                            true
                        },
                    )
                }
            }
            .pointerInput(controller, disabled) {
                if (disabled != null) return@pointerInput
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    val pointer = when (down.type) {
                        PointerType.Mouse -> PointerKind.MOUSE
                        PointerType.Stylus, PointerType.Eraser -> PointerKind.PEN
                        else -> PointerKind.TOUCH
                    }
                    controller.press(pointer, down.position.x / density, down.position.y / density)
                    down.consume()
                    var ended = false
                    try {
                        while (true) {
                            val event = awaitPointerEvent()
                            val change = event.changes.firstOrNull { it.id == down.id } ?: break
                            if (change.changedToUp()) {
                                change.consume()
                                controller.release()
                                ended = true
                                break
                            }
                            if (change.positionChanged()) {
                                controller.move(change.position.x / density, change.position.y / density)
                            }
                            change.consume()
                        }
                    } finally {
                        if (!ended) controller.gestureCancelled()
                    }
                }
            },
    ) {
        Icon(
            imageVector = if (kind == CaptureKind.VOICE) Icons.Filled.Mic else Icons.Filled.Videocam,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onPrimary,
        )
    }
    }
}

/**
 * What replaces the text field while a take is live: the recording bar
 * (§2 flow 2), the locked bar with Stop / Delete (§2 flow 5), or the voice
 * preview (§2 flow 6). A video note's preview is drawn by [VideoNoteOverlay];
 * this bar then only carries Delete / Send.
 */
@Composable
internal fun RecordingBar(controller: RecordController, modifier: Modifier = Modifier) {
    val density = LocalDensity.current.density
    when (val s = controller.state) {
        is RecState.Recording -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = modifier.fillMaxWidth().padding(horizontal = 8.dp),
        ) {
            PulsingDot()
            Spacer(Modifier.width(8.dp))
            Text(formatClock(controller.elapsedMs), style = MaterialTheme.typography.bodyMedium)
            Spacer(Modifier.width(8.dp))
            LevelMeter(controller.level, Modifier.width(40.dp).height(16.dp))
            Spacer(Modifier.weight(1f))
            // §2 "Hearing nothing" — IN PLACE OF the hint, not beside it: two
            // competing lines in one slot would push the slide affordance around
            // mid-gesture, and the warning is the more urgent of the two.
            if (controller.micSilent) {
                SilentMicNotice()
            } else {
                Text(
                    text = "< ${RecordCopy.SLIDE_TO_CANCEL}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.offset { IntOffset((minOf(0f, s.dx) * density).toInt(), 0) },
                )
            }
        }

        is RecState.Locked -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = modifier.fillMaxWidth().padding(horizontal = 4.dp),
        ) {
            IconButton(onClick = controller::delete) {
                Icon(Icons.Filled.Delete, contentDescription = "Cancel recording")
            }
            PulsingDot()
            Spacer(Modifier.width(8.dp))
            Text(
                text = formatClock(controller.elapsedMs) + " / " + formatClock(capFor(s.kind)),
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.weight(1f))
            // A locked take is the one most worth warning about: the finger is
            // off the button and it runs until it is stopped, so a silent one
            // can waste minutes rather than seconds. Never reached by a video
            // note, whose level is not sampled at all, so it cannot be accused.
            if (controller.micSilent) {
                SilentMicNotice()
                Spacer(Modifier.width(8.dp))
            }
            if (s.kind == CaptureKind.VIDEO_NOTE && controller.canFlip) {
                IconButton(onClick = controller::flipCamera) {
                    Icon(Icons.Filled.Cameraswitch, contentDescription = "Flip camera")
                }
            }
            IconButton(onClick = controller::stop) {
                Icon(Icons.Filled.Stop, contentDescription = "Stop recording")
            }
        }

        is RecState.Finalizing -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = modifier.fillMaxWidth().padding(horizontal = 8.dp),
        ) {
            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }

        is RecState.Preview -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = modifier.fillMaxWidth().padding(horizontal = 4.dp),
        ) {
            IconButton(onClick = controller::delete) {
                Icon(Icons.Filled.Delete, contentDescription = "Delete recording")
            }
            val take = controller.preview
            if (s.kind == CaptureKind.VOICE && take != null) {
                VoiceTakePreview(take, Modifier.weight(1f))
            } else {
                Text(
                    text = formatTotalClock(s.durationMs),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.weight(1f),
                )
            }
            TextButton(onClick = controller::sendPreview) { Text("Send") }
        }

        else -> Unit
    }
}

/**
 * §2 "Hearing nothing" — the one place the warning is drawn, so the recording
 * and locked bars cannot word or colour it two ways. An advisory colour rather
 * than `error`: nothing has failed and nothing is being refused.
 */
@Composable
private fun SilentMicNotice() {
    Text(
        text = RecordCopy.NO_MIC_SOUND,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun PulsingDot() {
    val transition = rememberInfiniteTransition(label = "rec-dot")
    val a by transition.animateFloat(
        initialValue = 1f,
        targetValue = 0.3f,
        animationSpec = infiniteRepeatable(tween(700), RepeatMode.Reverse),
        label = "rec-dot-alpha",
    )
    Box(
        Modifier
            .size(10.dp)
            .alpha(a)
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.error),
    )
}

@Composable
private fun LevelMeter(level: Float, modifier: Modifier) {
    Box(modifier.clip(RoundedCornerShape(4.dp)).background(MaterialTheme.colorScheme.surfaceVariant)) {
        Box(
            Modifier
                .fillMaxSize()
                .padding(end = ((1f - (level * 3f).coerceIn(0f, 1f)) * 40).dp)
                .background(MaterialTheme.colorScheme.error.copy(alpha = 0.7f)),
        )
    }
}

/** §2 flow 6 — play/pause the take, waveform, duration. No trimming. */
@Composable
private fun VoiceTakePreview(take: RecordedTake, modifier: Modifier) {
    var playing by remember(take) { mutableStateOf(false) }
    var position by remember(take) { mutableLongStateOf(0L) }
    val player = remember(take) { android.media.MediaPlayer() }
    DisposableEffect(take) {
        runCatching {
            player.setDataSource(take.file.absolutePath)
            player.prepare()
            player.setOnCompletionListener {
                playing = false
                position = 0
            }
        }
        onDispose { runCatching { player.release() } }
    }
    LaunchedEffect(playing) {
        if (playing) runCatching { player.start() } else runCatching { if (player.isPlaying) player.pause() }
        while (playing) {
            position = runCatching { player.currentPosition.toLong() }.getOrDefault(position)
            delay(100)
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically, modifier = modifier) {
        IconButton(onClick = { playing = !playing }) {
            Icon(
                if (playing) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                contentDescription = if (playing) "Pause" else "Play recording, ${spokenDuration(take.durationMs)}",
            )
        }
        WaveformSeekBar(
            waveform = take.waveform,
            progress = if (take.durationMs > 0) position.toFloat() / take.durationMs else 0f,
            playedColor = MaterialTheme.colorScheme.primary,
            idleColor = MaterialTheme.colorScheme.outlineVariant,
            label = "Recording position",
            modifier = Modifier.weight(1f).height(28.dp),
            onSeek = { f ->
                val target = (take.durationMs * f).toLong()
                runCatching { player.seekTo(target.toInt()) }
                position = target
            },
        )
        Spacer(Modifier.width(6.dp))
        Text(formatTotalClock(take.durationMs), style = MaterialTheme.typography.labelMedium)
    }
}

/**
 * §3 flow 1 — the large circular live preview of the front camera over the
 * chat, with a progress ring counting toward the cap and `Slide to cancel`.
 * In preview the round clip loops muted; a tap plays it with sound.
 */
@Composable
internal fun VideoNoteOverlay(controller: RecordController, modifier: Modifier = Modifier) {
    val s = controller.state
    val kind = when (s) {
        is RecState.Recording -> s.kind
        is RecState.Locked -> s.kind
        is RecState.Finalizing -> s.kind
        is RecState.Preview -> s.kind
        else -> null
    }
    if (kind != CaptureKind.VIDEO_NOTE) return
    val cap = capFor(CaptureKind.VIDEO_NOTE)
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.35f)),
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Box(contentAlignment = Alignment.Center, modifier = Modifier.size(280.dp)) {
                val take = controller.preview
                if (s is RecState.Preview && take != null) {
                    var withSound by remember(take) { mutableStateOf(false) }
                    VideoSurface(
                        file = take.file,
                        playing = true,
                        muted = !withSound,
                        looping = !withSound,
                        onCompleted = { withSound = false },
                        modifier = Modifier
                            .size(260.dp)
                            .clip(CircleShape)
                            .clickable(onClickLabel = "Play with sound") { withSound = !withSound },
                    )
                } else {
                    AndroidView(
                        factory = { ctx ->
                            PreviewView(ctx).apply {
                                // TextureView, so the circle clip applies.
                                implementationMode = PreviewView.ImplementationMode.COMPATIBLE
                                scaleType = PreviewView.ScaleType.FILL_CENTER
                                controller.attachPreview(this)
                            }
                        },
                        // Carries the view, so the controller clears it only
                        // when it is still the one it is using — a sibling
                        // `DisposableEffect` could only blank it blindly.
                        onRelease = { controller.detachPreview(it) },
                        modifier = Modifier.size(260.dp).clip(CircleShape),
                    )
                    if (controller.videoLive) {
                        CircularProgressIndicator(
                            progress = { (controller.elapsedMs.toFloat() / cap).coerceIn(0f, 1f) },
                            modifier = Modifier.size(276.dp),
                            strokeWidth = 4.dp,
                            color = MaterialTheme.colorScheme.error,
                        )
                    } else {
                        // The camera is still coming up: no progress ring and
                        // no running counter until the recording really runs.
                        CircularProgressIndicator(
                            modifier = Modifier.size(276.dp).semantics {
                                contentDescription = RecordCopy.STARTING_CAMERA
                            },
                            strokeWidth = 4.dp,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            if (s is RecState.Recording) {
                Spacer(Modifier.height(12.dp))
                Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surface) {
                    Text(
                        RecordCopy.SLIDE_TO_CANCEL,
                        style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
                    )
                }
            }
        }
    }
}

/**
 * §6 — the denied card above the composer, with Open Settings. A permanently
 * denied permission returns from the system request at once, so the settings
 * page is the only way back.
 */
@Composable
internal fun MediaDeniedCard(kind: CaptureKind, onDismiss: () -> Unit) {
    val context = LocalContext.current
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 4.dp)
            .border(0.dp, Color.Transparent, RoundedCornerShape(12.dp)),
    ) {
        Column(Modifier.padding(12.dp)) {
            Text(
                text = if (kind == CaptureKind.VOICE) RecordCopy.DENIED_MIC else RecordCopy.DENIED_CAMERA,
                style = MaterialTheme.typography.bodyMedium,
            )
            Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                TextButton(onClick = onDismiss) { Text("Not now") }
                TextButton(onClick = {
                    openAppSettings(context)
                    onDismiss()
                }) { Text("Open Settings") }
            }
        }
    }
}

private fun openAppSettings(context: Context) {
    val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
        .setData(Uri.fromParts("package", context.packageName, null))
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    runCatching { context.startActivity(intent) }
}
