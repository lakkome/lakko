// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * ADR-0047 / UX spec §2-§3, §6 — the hold-to-record gesture as a pure state
 * machine.
 *
 * Port of `apps/web/src/media/recording-machine.ts`, kept rule-for-rule so the
 * three clients agree on when a take starts, is sent, locks, is cancelled or is
 * thrown away. Every such rule lives in [reduce]; the Compose glue
 * (`ui/chat/MediaComposer.kt`) only translates pointer / timer / recorder
 * callbacks into [RecEvent]s and performs the [RecEffect]s this returns. That
 * split is what makes the rules testable on the host JVM, with no Android
 * classes and no recorder.
 *
 * The rules, and where they come from:
 *
 *   * §3.1 — the button answers TWO gestures, and the difference is decided ON
 *     THE GESTURE, BEFORE anything is captured, never on how long the take
 *     turned out to be. A press becomes a hold once it has lasted [TAP_MAX_MS]
 *     without travelling past [TAP_SLOP_PX], and only then is a permission
 *     asked for and a capture started ([RecState.Pressing]). Let go sooner, it
 *     is a tap and switches the mode; travel further, it is neither.
 *
 *     This is what the spec means by "a tap therefore never records". It also
 *     used to be the visible defect on this client: a tap went straight into
 *     [RecState.Recording], which swaps the composer for the recording bar and
 *     — in camera mode — mounts the full-screen round preview, so the whole
 *     screen flashed for the few tens of milliseconds a tap lasts.
 *   * Press and hold starts recording immediately once the press has armed
 *     and permission is known to be granted (§2 flow 1, §6).
 *   * Release sends (§2.3). Slide left past [CANCEL_SLIDE_PX] cancels (§2.4).
 *     Slide up past [LOCK_SLIDE_PX] locks (§2.5).
 *   * A take shorter than [MIN_TAKE_MS] is discarded with the `Hold to record`
 *     hint (§2.8), and is NOT a mode switch: §3.1 is explicit that the press
 *     had already opened the microphone, so it is the accidental-press case
 *     rather than the tap.
 *   * §3.1 — **no press ever ends in a locked recording**, whatever the
 *     pointer. An older desktop note let a short mouse press carry on
 *     hands-free; that gave the one gesture a second meaning, and it was the
 *     only pointer route to the camera before the mode switch existed. A mouse
 *     now takes exactly the paths a thumb does — too short to arm is a tap and
 *     switches the mode, armed-then-let-go-early is the accidental press — and
 *     reaches [RecState.Locked] by holding and sliding up onto the lock target,
 *     which a pointer reaches as readily as a thumb.
 *   * Accessibility activation (TalkBack double-tap, a switch, a keyboard)
 *     starts a locked recording — the single-activation path §7 requires,
 *     since hold-and-slide is not accessible. It is the ONE route to
 *     [RecState.Locked] that is not a hold, because it is not a press at all.
 *   * The cap ([VOICE_CAP_MS] 15 min, [VIDEO_NOTE_CAP_MS] 60 s) stops the take
 *     into preview (§2.7, §3.3).
 *   * The hold that triggered a permission PROMPT does not record (§6). On
 *     Android a runtime-permission request for an ungranted permission always
 *     shows a dialog (or returns at once when permanently denied), so the glue
 *     passes [PermissionHint.PROMPT] and the elapsed-time inference the web
 *     needs is never the deciding input here. It is kept so the reducer stays
 *     the same function on every client.
 *   * Permission revoked mid-take discards the take and shows the denied card.
 */

public enum class CaptureKind { VOICE, VIDEO_NOTE }

public enum class PointerKind { MOUSE, TOUCH, PEN }

/** What the platform reports before any capture is attempted. */
public enum class PermissionHint { GRANTED, PROMPT, DENIED, UNKNOWN }

/** §2.7 — the voice cap. */
public const val VOICE_CAP_MS: Long = 15L * 60L * 1000L

/** §3.3 — the video-note cap. */
public const val VIDEO_NOTE_CAP_MS: Long = 60L * 1000L

/** §2.8 — shorter takes are discarded (touch) or read as a click (mouse). */
public const val MIN_TAKE_MS: Long = 500L

/**
 * §3.1 — how long a press must last before it stops being a tap.
 *
 * A tap switches the mode; a hold records. Nothing is captured and no
 * permission is asked for until this has elapsed with the pointer still down,
 * which is what makes "a tap records nothing" true rather than "a tap records
 * and throws it away".
 *
 * The number is a choice, not a measurement, and it sits between the two the
 * platform offers: `ViewConfiguration`'s tap timeout (100 ms) is inside the
 * jitter of a deliberate press, and its long-press timeout (500 ms) is
 * [MIN_TAKE_MS], so arming there would make the shortest sendable take a
 * full second of holding. It is deliberately well under [MIN_TAKE_MS] so the
 * two thresholds stay independent: arming decides tap-or-hold, and
 * [MIN_TAKE_MS] decides whether an armed take is worth sending.
 */
public const val TAP_MAX_MS: Long = 200L

/**
 * §3.1 — how far a press may travel and still be judged tap-or-hold.
 *
 * A press that wanders past this is **neither**: not the mode switch, because a
 * finger on its way to the cancel target must not flip the mode when it is let
 * go, and not a take either, because nothing was ever opened. It simply ends.
 *
 * Far below [CANCEL_SLIDE_PX] and [LOCK_SLIDE_PX], which act on a running take;
 * this one only says the press was a drag rather than a press.
 */
public const val TAP_SLOP_PX: Float = 10f

/** §2.4 — how far left (in px) a held pointer travels before the take cancels. */
public const val CANCEL_SLIDE_PX: Float = 80f

/** §2.5 — how far up (in px) a held pointer travels before the take locks. */
public const val LOCK_SLIDE_PX: Float = 60f

/** A permission grant slower than this is assumed to have shown a prompt. */
public const val PROMPT_INFERENCE_MS: Long = 1000L

public fun capFor(kind: CaptureKind): Long =
    if (kind == CaptureKind.VOICE) VOICE_CAP_MS else VIDEO_NOTE_CAP_MS

/** The press that is being resolved while a permission request is out. */
public enum class RequestSource { MOUSE, TOUCH, PEN, ACCESSIBILITY }

public sealed interface RecState {
    public data object Idle : RecState

    /**
     * §3.1 — the pointer is down and NOTHING has been captured: no microphone,
     * no camera, no permission prompt. The press is still a tap, and becomes a
     * hold at [TAP_MAX_MS] via [RecEvent.HoldElapsed].
     *
     * No UI state belongs to it. The composer must look exactly as it does in
     * [Idle] while a press is being judged, or a tap flashes the recording UI
     * — see [showsRecordingUi].
     */
    public data class Pressing(
        val kind: CaptureKind,
        val pointer: PointerKind,
        val pressedAt: Long,
        val originX: Float,
        val originY: Float,
    ) : RecState

    public data class Requesting(
        val kind: CaptureKind,
        val source: RequestSource,
        val pressedAt: Long,
        /** The pointer is still down. */
        val held: Boolean,
        /** The platform said `prompt` up front. */
        val knownPrompt: Boolean,
        val originX: Float,
        val originY: Float,
    ) : RecState

    public data class Recording(
        val kind: CaptureKind,
        val pointer: PointerKind,
        val startedAt: Long,
        val originX: Float,
        val originY: Float,
        /** Horizontal offset from the press point (negative = left), for the
         *  "slide to cancel" feedback. */
        val dx: Float,
        val dy: Float,
    ) : RecState

    public data class Locked(val kind: CaptureKind, val startedAt: Long) : RecState

    /** Waiting for the recorder to hand over the finished take. */
    public data class Finalizing(val kind: CaptureKind) : RecState

    public data class Preview(val kind: CaptureKind, val durationMs: Long) : RecState
}

public sealed interface RecEvent {
    /**
     * The pointer went down. It carries no permission hint: a press on its own
     * asks for nothing and captures nothing, so there is nothing to decide
     * yet. The hint arrives with [HoldElapsed], read at the moment it matters.
     */
    public data class Press(
        val kind: CaptureKind,
        val pointer: PointerKind,
        val x: Float,
        val y: Float,
        val at: Long,
    ) : RecEvent

    /**
     * §3.1 — the arming timer fired with the pointer still down, so the press
     * is a hold. This is the first moment anything may be asked for or opened.
     */
    public data class HoldElapsed(val at: Long, val permission: PermissionHint) : RecEvent

    public data class Move(val x: Float, val y: Float) : RecEvent

    public data class Release(val at: Long) : RecEvent

    /** Accessibility activation of the record button. */
    public data class Activate(
        val kind: CaptureKind,
        val at: Long,
        val permission: PermissionHint,
    ) : RecEvent

    public data class PermissionResult(val granted: Boolean, val at: Long) : RecEvent

    public data class Tick(val at: Long) : RecEvent

    public data class Stop(val at: Long) : RecEvent

    public data object Delete : RecEvent

    /** Back press, or the gesture was cancelled by the system mid-hold. */
    public data object Cancel : RecEvent

    public data object Send : RecEvent

    public data object Revoked : RecEvent

    public data object CaptureFailed : RecEvent

    public data class PreviewReady(val durationMs: Long) : RecEvent

    public data object PreviewFailed : RecEvent
}

public enum class RecHint { HOLD_TO_RECORD, CANCELLED }

public enum class FinishDisposition { SEND, PREVIEW, DISCARD }

public sealed interface RecEffect {
    public data class RequestPermission(val kind: CaptureKind) : RecEffect

    public data class StartCapture(val kind: CaptureKind) : RecEffect

    /** Nothing was captured on behalf of a prompt; nothing to release on
     *  Android, kept for parity with the web reducer's effect list. */
    public data object ReleaseStream : RecEffect

    public data class Finish(val disposition: FinishDisposition) : RecEffect

    public data class Hint(val hint: RecHint) : RecEffect

    public data class ShowDenied(val kind: CaptureKind) : RecEffect

    public data object ToggleMode : RecEffect

    public data object SendPreview : RecEffect

    public data object DiscardPreview : RecEffect
}

public data class RecStep(val state: RecState, val effects: List<RecEffect>)

private fun step(state: RecState, vararg effects: RecEffect): RecStep =
    RecStep(state, effects.toList())

/** Is a capture running in this state? */
public fun isCapturing(state: RecState): Boolean =
    state is RecState.Recording || state is RecState.Locked

/**
 * Does the recording UI replace the composer in this state?
 *
 * Written down here rather than as a condition at the composer, because the
 * states where it must NOT is the whole of §3.1's "a tap records nothing": a
 * press being judged ([RecState.Pressing]) and a permission being asked for
 * ([RecState.Requesting]) both look exactly like [RecState.Idle], and a screen
 * that swaps its composer for either of them flashes on every tap.
 */
public fun showsRecordingUi(state: RecState): Boolean =
    state !is RecState.Idle && state !is RecState.Pressing && state !is RecState.Requesting

public fun reduce(state: RecState, event: RecEvent): RecStep = when (state) {
    RecState.Idle -> fromIdle(event)
    is RecState.Pressing -> fromPressing(state, event)
    is RecState.Requesting -> fromRequesting(state, event)
    is RecState.Recording -> fromRecording(state, event)
    is RecState.Locked -> fromLocked(state, event)
    is RecState.Finalizing -> fromFinalizing(state, event)
    is RecState.Preview -> fromPreview(state, event)
}

private fun PointerKind.asSource(): RequestSource = when (this) {
    PointerKind.MOUSE -> RequestSource.MOUSE
    PointerKind.TOUCH -> RequestSource.TOUCH
    PointerKind.PEN -> RequestSource.PEN
}

private fun RequestSource.asPointer(): PointerKind? = when (this) {
    RequestSource.MOUSE -> PointerKind.MOUSE
    RequestSource.TOUCH -> PointerKind.TOUCH
    RequestSource.PEN -> PointerKind.PEN
    RequestSource.ACCESSIBILITY -> null
}

private fun fromIdle(event: RecEvent): RecStep = when (event) {
    // §3.1 — a press decides nothing yet. No permission is read, no card is
    // shown, no capture starts: it is a tap until it has been held.
    is RecEvent.Press -> step(
        RecState.Pressing(
            kind = event.kind,
            pointer = event.pointer,
            pressedAt = event.at,
            originX = event.x,
            originY = event.y,
        ),
    )

    is RecEvent.Activate -> when (event.permission) {
        PermissionHint.DENIED -> step(RecState.Idle, RecEffect.ShowDenied(event.kind))
        PermissionHint.GRANTED -> step(
            RecState.Locked(event.kind, event.at),
            RecEffect.StartCapture(event.kind),
        )
        else -> step(
            RecState.Requesting(
                kind = event.kind,
                source = RequestSource.ACCESSIBILITY,
                pressedAt = event.at,
                held = false,
                knownPrompt = event.permission == PermissionHint.PROMPT,
                originX = 0f,
                originY = 0f,
            ),
            RecEffect.RequestPermission(event.kind),
        )
    }

    else -> step(RecState.Idle)
}

/**
 * §3.1 — the press is being judged, and the judgement is made ON THE GESTURE,
 * before anything is captured. Three ways out: it is let go, which makes it a
 * tap; it outlives [TAP_MAX_MS] in place, which makes it a hold; or it travels
 * past [TAP_SLOP_PX], which makes it neither.
 *
 * Judging it on the outcome instead — start recording and decide afterwards
 * whether the take was long enough — is what opened the microphone on every tap
 * and flashed the recording UI while doing it.
 */
private fun fromPressing(state: RecState.Pressing, event: RecEvent): RecStep = when (event) {
    // The tap. Nothing was captured, so there is nothing to discard and no
    // `Hold to record` hint either: the person did not fail to hold a
    // recording, they asked for the other mode.
    is RecEvent.Release -> step(RecState.Idle, RecEffect.ToggleMode)

    // Past the slop the press is neither gesture, and it ends here rather than
    // waiting: going idle is also what stops a later [RecEvent.HoldElapsed]
    // from arming a take the finger has already walked away from.
    is RecEvent.Move ->
        if (travelledPastSlop(state, event)) step(RecState.Idle) else step(state)

    RecEvent.Cancel, RecEvent.Revoked -> step(RecState.Idle)
    is RecEvent.HoldElapsed -> armedHold(state, event)
    else -> step(state)
}

/** Straight-line distance from the press point, against [TAP_SLOP_PX]. */
private fun travelledPastSlop(state: RecState.Pressing, event: RecEvent.Move): Boolean {
    val dx = event.x - state.originX
    val dy = event.y - state.originY
    return dx * dx + dy * dy > TAP_SLOP_PX * TAP_SLOP_PX
}

/**
 * The press outlived [TAP_MAX_MS]: it is a hold, and this is the first moment
 * a permission may be asked for or a microphone opened.
 *
 * The take is timed from [RecEvent.HoldElapsed.at] rather than from the press,
 * so [MIN_TAKE_MS] keeps measuring recorded audio rather than finger time.
 */
private fun armedHold(state: RecState.Pressing, event: RecEvent.HoldElapsed): RecStep =
    when (event.permission) {
        PermissionHint.DENIED -> step(RecState.Idle, RecEffect.ShowDenied(state.kind))
        PermissionHint.GRANTED -> step(
            RecState.Recording(
                kind = state.kind,
                pointer = state.pointer,
                startedAt = event.at,
                originX = state.originX,
                originY = state.originY,
                dx = 0f,
                dy = 0f,
            ),
            RecEffect.StartCapture(state.kind),
        )
        else -> step(
            RecState.Requesting(
                kind = state.kind,
                source = state.pointer.asSource(),
                pressedAt = event.at,
                held = true,
                knownPrompt = event.permission == PermissionHint.PROMPT,
                originX = state.originX,
                originY = state.originY,
            ),
            RecEffect.RequestPermission(state.kind),
        )
    }

private fun fromRequesting(state: RecState.Requesting, event: RecEvent): RecStep = when (event) {
    is RecEvent.Release -> step(state.copy(held = false))
    // Nothing is recording yet; the grant that may still arrive must not start
    // one. Going idle does that: a PermissionResult is ignored there.
    RecEvent.Cancel -> step(RecState.Idle)
    is RecEvent.PermissionResult -> requestingResolved(state, event)
    else -> step(state)
}

private fun requestingResolved(
    state: RecState.Requesting,
    event: RecEvent.PermissionResult,
): RecStep {
    if (!event.granted) return step(RecState.Idle, RecEffect.ShowDenied(state.kind))
    val prompted = state.knownPrompt || event.at - state.pressedAt > PROMPT_INFERENCE_MS
    // §6: the gesture that raised the prompt does not record.
    if (prompted) return step(RecState.Idle, RecEffect.ReleaseStream)
    val pointer = state.source.asPointer()
        ?: return step(
            RecState.Locked(state.kind, event.at),
            RecEffect.StartCapture(state.kind),
        )
    if (state.held) {
        return step(
            RecState.Recording(
                kind = state.kind,
                pointer = pointer,
                startedAt = event.at,
                originX = state.originX,
                originY = state.originY,
                dx = 0f,
                dy = 0f,
            ),
            RecEffect.StartCapture(state.kind),
        )
    }
    // Released before the (prompt-less) grant arrived: a quick press. Not a
    // mode switch — §3.1 — because the press had already armed and asked for
    // the microphone, so it is the accidental-press case of §2.8. A mouse takes
    // the same exit as a thumb: §3.1's "no press ever ends in a locked
    // recording".
    return step(
        RecState.Idle,
        RecEffect.ReleaseStream,
        RecEffect.Hint(RecHint.HOLD_TO_RECORD),
    )
}

private fun fromRecording(state: RecState.Recording, event: RecEvent): RecStep = when (event) {
    is RecEvent.Move -> {
        val dx = event.x - state.originX
        val dy = event.y - state.originY
        when {
            dx <= -CANCEL_SLIDE_PX -> step(
                RecState.Idle,
                RecEffect.Finish(FinishDisposition.DISCARD),
                RecEffect.Hint(RecHint.CANCELLED),
            )
            dy <= -LOCK_SLIDE_PX -> step(RecState.Locked(state.kind, state.startedAt))
            else -> step(state.copy(dx = dx, dy = dy))
        }
    }

    is RecEvent.Release -> when {
        event.at - state.startedAt >= MIN_TAKE_MS ->
            step(RecState.Idle, RecEffect.Finish(FinishDisposition.SEND))
        // §3.1 — the accidental press, and NOT a mode switch: the microphone
        // was already open, so the tap that switches the mode is the one that
        // never reached [RecState.Recording] at all. Every pointer takes this
        // exit — see §3.1's "no press ever ends in a locked recording".
        else -> step(
            RecState.Idle,
            RecEffect.Finish(FinishDisposition.DISCARD),
            RecEffect.Hint(RecHint.HOLD_TO_RECORD),
        )
    }

    is RecEvent.Tick ->
        if (event.at - state.startedAt >= capFor(state.kind)) {
            step(
                RecState.Finalizing(state.kind),
                RecEffect.Finish(FinishDisposition.PREVIEW),
            )
        } else {
            step(state)
        }

    RecEvent.Cancel -> step(
        RecState.Idle,
        RecEffect.Finish(FinishDisposition.DISCARD),
        RecEffect.Hint(RecHint.CANCELLED),
    )

    RecEvent.Delete -> step(RecState.Idle, RecEffect.Finish(FinishDisposition.DISCARD))
    RecEvent.Revoked -> step(
        RecState.Idle,
        RecEffect.Finish(FinishDisposition.DISCARD),
        RecEffect.ShowDenied(state.kind),
    )
    RecEvent.CaptureFailed -> step(RecState.Idle, RecEffect.Finish(FinishDisposition.DISCARD))
    else -> step(state)
}

private fun fromLocked(state: RecState.Locked, event: RecEvent): RecStep = when (event) {
    is RecEvent.Stop ->
        if (event.at - state.startedAt < MIN_TAKE_MS) {
            step(
                RecState.Idle,
                RecEffect.Finish(FinishDisposition.DISCARD),
                RecEffect.Hint(RecHint.HOLD_TO_RECORD),
            )
        } else {
            step(RecState.Finalizing(state.kind), RecEffect.Finish(FinishDisposition.PREVIEW))
        }

    is RecEvent.Tick ->
        if (event.at - state.startedAt >= capFor(state.kind)) {
            step(RecState.Finalizing(state.kind), RecEffect.Finish(FinishDisposition.PREVIEW))
        } else {
            step(state)
        }

    RecEvent.Delete, RecEvent.Cancel ->
        step(RecState.Idle, RecEffect.Finish(FinishDisposition.DISCARD))
    RecEvent.Revoked -> step(
        RecState.Idle,
        RecEffect.Finish(FinishDisposition.DISCARD),
        RecEffect.ShowDenied(state.kind),
    )
    RecEvent.CaptureFailed -> step(RecState.Idle, RecEffect.Finish(FinishDisposition.DISCARD))
    else -> step(state)
}

private fun fromFinalizing(state: RecState.Finalizing, event: RecEvent): RecStep = when (event) {
    is RecEvent.PreviewReady ->
        if (event.durationMs < MIN_TAKE_MS) {
            step(RecState.Idle, RecEffect.DiscardPreview, RecEffect.Hint(RecHint.HOLD_TO_RECORD))
        } else {
            step(RecState.Preview(state.kind, event.durationMs))
        }
    RecEvent.PreviewFailed, RecEvent.CaptureFailed -> step(RecState.Idle)
    else -> step(state)
}

private fun fromPreview(state: RecState.Preview, event: RecEvent): RecStep = when (event) {
    RecEvent.Send -> step(RecState.Idle, RecEffect.SendPreview)
    RecEvent.Delete, RecEvent.Cancel -> step(RecState.Idle, RecEffect.DiscardPreview)
    else -> step(state)
}

/** What a released (SEND) take becomes once the recorder has stopped. */
public enum class SentTakeOutcome { SEND, HOLD_TO_RECORD, START_FAILED }

/**
 * §2.8, applied AFTER the recorder stopped. The reducer judged the hold from
 * the press; the recorder may have spent part of it starting (a slow
 * `MediaRecorder.start`), so the take it hands back is judged again:
 *
 *   * a take of at least [MIN_TAKE_MS] is sent;
 *   * a shorter take, or none at all from a capture that DID start (stopped
 *     before any audio was written), is a too-short hold: `Hold to record`;
 *   * no take from a capture that never started is a start failure.
 */
public fun sentTakeOutcome(takeDurationMs: Long?, captureStarted: Boolean): SentTakeOutcome = when {
    takeDurationMs == null ->
        if (captureStarted) SentTakeOutcome.HOLD_TO_RECORD else SentTakeOutcome.START_FAILED
    takeDurationMs < MIN_TAKE_MS -> SentTakeOutcome.HOLD_TO_RECORD
    else -> SentTakeOutcome.SEND
}

/** `0:07`, `1:02`, `15:00` — the recording bar and bubble duration format. */
public fun formatClock(ms: Long): String {
    val total = maxOf(0L, ms / 1000L)
    val minutes = total / 60L
    val seconds = total % 60L
    return "$minutes:${seconds.toString().padStart(2, '0')}"
}

/**
 * A TOTAL duration (a voice message or video note as a whole): any positive
 * length rounds UP to whole seconds, so a 0.7 s take reads `0:01`, not `0:00`.
 * Zero or negative reads `0:00`. Live / elapsed counters keep [formatClock].
 */
public fun formatTotalClock(ms: Long): String {
    if (ms <= 0L) return formatClock(0L)
    val seconds = ms / 1000L + if (ms % 1000L == 0L) 0L else 1L
    return formatClock(seconds * 1000L)
}

/** Spoken duration for labels: "12 seconds", "1 minute 5 seconds". */
public fun spokenDuration(ms: Long): String {
    val total = maxOf(0L, Math.round(ms / 1000.0))
    val minutes = total / 60L
    val seconds = total % 60L
    val s = "$seconds ${if (seconds == 1L) "second" else "seconds"}"
    if (minutes == 0L) return s
    val m = "$minutes ${if (minutes == 1L) "minute" else "minutes"}"
    return if (seconds == 0L) m else "$m $s"
}
