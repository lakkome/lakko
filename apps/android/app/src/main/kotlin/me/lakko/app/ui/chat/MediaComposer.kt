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
import androidx.compose.ui.semantics.contentDescription
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
import me.lakko.app.media.SentTakeOutcome
import me.lakko.app.media.sentTakeOutcome
import me.lakko.app.media.VideoNoteRecorder
import me.lakko.app.media.VoiceRecorder
import me.lakko.app.media.capFor
import me.lakko.app.media.formatClock
import me.lakko.app.media.formatTotalClock
import me.lakko.app.media.isCapturing
import me.lakko.app.media.reduce
import me.lakko.app.media.spokenDuration
import java.util.UUID

private const val TAG = "LakkoRecordController"
private const val TICK_MS = 100L
private const val NOTICE_MS = 2_500L

/** UX spec §2 / §3 / §6 copy. */
internal object RecordCopy {
    const val HOLD_TO_RECORD = "Hold to record"
    const val CANCELLED = "Cancelled"
    const val NO_CAMERA = "No camera found"
    const val START_FAILED = "Couldn't start recording"
    const val IN_CALL = "Unavailable during a call"
    const val SLIDE_TO_CANCEL = "Slide to cancel"
    const val DENIED_MIC = "Lakko needs microphone access to record voice messages"
    const val DENIED_CAMERA = "Lakko needs camera access to record video messages"
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
    var notice by mutableStateOf<String?>(null)
        private set
    var denied by mutableStateOf<CaptureKind?>(null)
        private set
    var preview by mutableStateOf<RecordedTake?>(null)
        private set
    var mode by mutableStateOf(hub.prefs.recordMode)
        private set
    var frontFacing by mutableStateOf(true)
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

    private val cameraPresent: Boolean = VideoNoteRecorder.hasAnyCamera(context)
    internal var requestPermissions: ((Array<String>) -> Unit)? = null

    private var voice: VoiceRecorder? = null
    /** The voice recorder's start, running on [RecorderThread]; `true` once it
     *  is recording. Everything that touches [voice] afterwards awaits it, so
     *  a release that lands while start is still blocked is applied after. */
    private var voiceStart: Deferred<Boolean>? = null
    private var video: VideoNoteRecorder? = null
    private var previewView: PreviewView? = null
    private var tickJob: Job? = null
    private var noticeJob: Job? = null
    private var startJob: Job? = null
    private var captureStartedAt: Long = 0L

    init {
        if (mode == CaptureKind.VIDEO_NOTE && !cameraPresent) mode = CaptureKind.VOICE
    }

    private fun now(): Long = SystemClock.elapsedRealtime()

    /** Why the record button is disabled, or `null` when it is usable. */
    val disabledReason: String? get() = if (inCall) RecordCopy.IN_CALL else null

    fun press(pointer: PointerKind, xDp: Float, yDp: Float) {
        if (disabledReason != null) return
        dispatch(RecEvent.Press(mode, pointer, xDp, yDp, now(), permissionHint(mode)))
    }

    fun move(xDp: Float, yDp: Float) = dispatch(RecEvent.Move(xDp, yDp))

    fun release() = dispatch(RecEvent.Release(now()))

    /** The system took the gesture away mid-hold. */
    fun gestureCancelled() {
        if (state is RecState.Recording || state is RecState.Requesting) dispatch(RecEvent.Cancel)
    }

    /** TalkBack / switch access: a single activation starts a LOCKED take. */
    fun activate() {
        if (disabledReason != null) return
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
        if (v.flip()) frontFacing = v.frontFacing
    }

    fun attachPreview(view: PreviewView?) {
        previewView = view
    }

    fun onPermissionResult() {
        val granted = requiredPermissions(currentKind() ?: mode).all(::isGranted)
        dispatch(RecEvent.PermissionResult(granted, now()))
    }

    private fun currentKind(): CaptureKind? = when (val s = state) {
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
                showNotice(RecordCopy.NO_CAMERA)
                return
            }
            mode = CaptureKind.VIDEO_NOTE
        } else {
            mode = CaptureKind.VOICE
        }
        hub.prefs.recordMode = mode
    }

    private fun showNotice(text: String) {
        notice = text
        noticeJob?.cancel()
        noticeJob = scope.launch {
            delay(NOTICE_MS)
            notice = null
        }
    }

    private fun startCapture(kind: CaptureKind) {
        elapsedMs = 0L
        level = 0f
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
            val rec = VideoNoteRecorder(context, lifecycleOwner)
            video = rec
            frontFacing = rec.frontFacing
            startTick()
            startJob = scope.launch {
                // The round preview composes once the state is Recording /
                // Locked; wait (briefly) for its PreviewView.
                val view = withTimeoutOrNull(PREVIEW_WAIT_MS) {
                    while (previewView == null && isActive) delay(20)
                    previewView
                }
                if (!isCapturing(state) || video !== rec) return@launch
                val file = hub.newEntryFile("take-${UUID.randomUUID()}", "video-note-$stamp.mp4")
                val ok = view != null && file != null && rec.bind(view) &&
                    rec.start(file) { scope.launch { dispatch(RecEvent.CaptureFailed) } }
                if (!ok) {
                    Log.w(TAG, "video note capture did not start")
                    showNotice(RecordCopy.START_FAILED)
                    dispatch(RecEvent.CaptureFailed)
                } else {
                    captureStartedAt = now()
                }
            }
        }
    }

    private fun startTick() {
        tickJob?.cancel()
        tickJob = scope.launch {
            while (isActive) {
                delay(TICK_MS)
                val kind = currentKind() ?: break
                val v = voice
                val starting = v != null && voiceStart?.isCompleted != true
                elapsedMs = if (starting) 0L else now() - captureStartedAt
                if (v != null && !starting) {
                    level = withContext(RecorderThread.dispatcher) { v.sampleLevel() }.toFloat()
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
        voice = null
        voiceStart = null
        video = null
        val elapsed = now() - captureStartedAt
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
 * The composer's right-hand record button: mic or camera. Hold to record,
 * slide left to cancel, slide up to lock; a single TalkBack activation starts a
 * locked take (§7). Coordinates are fed to the reducer in dp, so its slide
 * thresholds mean the same physical distance on every screen density.
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
            Text(
                text = "< ${RecordCopy.SLIDE_TO_CANCEL}",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.offset { IntOffset((minOf(0f, s.dx) * density).toInt(), 0) },
            )
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
            if (s.kind == CaptureKind.VIDEO_NOTE) {
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
                    DisposableEffect(Unit) {
                        onDispose { controller.attachPreview(null) }
                    }
                    AndroidView(
                        factory = { ctx ->
                            PreviewView(ctx).apply {
                                // TextureView, so the circle clip applies.
                                implementationMode = PreviewView.ImplementationMode.COMPATIBLE
                                scaleType = PreviewView.ScaleType.FILL_CENTER
                                controller.attachPreview(this)
                            }
                        },
                        modifier = Modifier.size(260.dp).clip(CircleShape),
                    )
                    CircularProgressIndicator(
                        progress = { (controller.elapsedMs.toFloat() / cap).coerceIn(0f, 1f) },
                        modifier = Modifier.size(276.dp),
                        strokeWidth = 4.dp,
                        color = MaterialTheme.colorScheme.error,
                    )
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
