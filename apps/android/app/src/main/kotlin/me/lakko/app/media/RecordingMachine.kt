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
 *   * Press and hold starts recording immediately, once permission is known
 *     to be granted (§2 flow 1, §6).
 *   * Release sends (§2.3). Slide left past [CANCEL_SLIDE_PX] cancels (§2.4).
 *     Slide up past [LOCK_SLIDE_PX] locks (§2.5).
 *   * A take shorter than [MIN_TAKE_MS] is discarded with the `Hold to record`
 *     hint (§2.8) for a touch or pen press. A mouse press that short is a
 *     click, and a click starts a LOCKED recording (the §2 desktop note; a
 *     mouse on ChromeOS or a tablet takes the same path).
 *   * A short touch tap also toggles mic / camera mode (§1).
 *   * Accessibility activation (TalkBack double-tap, a switch, a keyboard)
 *     starts a locked recording — the single-activation path §7 requires,
 *     since hold-and-slide is not accessible.
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
    public data class Press(
        val kind: CaptureKind,
        val pointer: PointerKind,
        val x: Float,
        val y: Float,
        val at: Long,
        val permission: PermissionHint,
    ) : RecEvent

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

public fun reduce(state: RecState, event: RecEvent): RecStep = when (state) {
    RecState.Idle -> fromIdle(event)
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
    is RecEvent.Press -> when (event.permission) {
        PermissionHint.DENIED -> step(RecState.Idle, RecEffect.ShowDenied(event.kind))
        PermissionHint.GRANTED -> step(
            RecState.Recording(
                kind = event.kind,
                pointer = event.pointer,
                startedAt = event.at,
                originX = event.x,
                originY = event.y,
                dx = 0f,
                dy = 0f,
            ),
            RecEffect.StartCapture(event.kind),
        )
        else -> step(
            RecState.Requesting(
                kind = event.kind,
                source = event.pointer.asSource(),
                pressedAt = event.at,
                held = true,
                knownPrompt = event.permission == PermissionHint.PROMPT,
                originX = event.x,
                originY = event.y,
            ),
            RecEffect.RequestPermission(event.kind),
        )
    }

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
    // Released before the (prompt-less) grant arrived: a quick press.
    if (pointer == PointerKind.MOUSE) {
        return step(RecState.Locked(state.kind, event.at), RecEffect.StartCapture(state.kind))
    }
    return step(
        RecState.Idle,
        RecEffect.ReleaseStream,
        RecEffect.Hint(RecHint.HOLD_TO_RECORD),
        RecEffect.ToggleMode,
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
        // A click: the take already running becomes a locked recording.
        state.pointer == PointerKind.MOUSE -> step(RecState.Locked(state.kind, state.startedAt))
        else -> step(
            RecState.Idle,
            RecEffect.Finish(FinishDisposition.DISCARD),
            RecEffect.Hint(RecHint.HOLD_TO_RECORD),
            RecEffect.ToggleMode,
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

/** `0:07`, `1:02`, `15:00` — the recording bar and bubble duration format. */
public fun formatClock(ms: Long): String {
    val total = maxOf(0L, ms / 1000L)
    val minutes = total / 60L
    val seconds = total % 60L
    return "$minutes:${seconds.toString().padStart(2, '0')}"
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
