// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The call screen's control row — `docs/ux/calls.md` sections 4.1 and 4.1.1.
 *
 * ## The defect this closes
 *
 * Section 4.1 names four faults, and the old row had all four:
 *
 *  - **The wrong kind of control.** The pre-call screen configured the call with
 *    switches in a settings list. "A switch says 'this is a preference you are
 *    configuring'. A call needs a button you press."
 *  - **Real actions behind an ellipsis menu.** Switching camera lived in a
 *    dropdown — "something a person does mid-sentence with one thumb; a menu
 *    makes it two taps and a reading task".
 *  - **Speaker was not on the screen at all**, though it is the control people
 *    reach for most on a voice call. It did not exist anywhere in the app.
 *  - **The highlighted button was the one that was off.** `CircleButton` filled
 *    with the accent colour when `active` was `local.micOn == false`, so a muted
 *    microphone looked like a live one.
 *
 * ## The rules this file encodes
 *
 * *Which* buttons exist ([callControlKinds]) and *how each reads*
 * ([callControlRow]). The Compose half draws them; nothing about the set or the
 * emphasis is decided in a composable, so all of it is unit-tested on the host
 * JVM.
 *
 * The row is the same for both kinds of call — Speaker, Mute, Camera, End —
 * because **the camera button is on a voice call too, and it turns the call
 * into a video one**. Section 4.1: "People start a call with their voice and
 * decide to show something a minute later; every messenger allows that, and
 * taking it away to keep the row short would be a feature lost to tidiness."
 *
 * That is why the row takes no "is this a video call" input. There is no such
 * thing on a live call: there is a camera that is sending or not, and the
 * button that changes which. Deriving the row from a call *kind* is what would
 * strand a voice call with no way to start showing something.
 *
 * A group call adds one overflow entry for participants and chat, "but mute,
 * camera, speaker and end stay in the row". Flip camera is deliberately absent
 * from every set: it belongs on the self-view rectangle, "where the thing it
 * changes is visible".
 */

/** One button of the row. Flip camera is not here; see the file header. */
public enum class CallControlKind {
    /** Route the call audio to the loudspeaker instead of the earpiece. */
    SPEAKER,

    /** Mute and unmute the microphone. */
    MICROPHONE,

    /** Start and stop sending the camera. */
    CAMERA,

    /** Group calls only: participants, chat, settings. */
    MORE,

    /** Hang up. The only red thing on the screen. */
    END,
}

/**
 * How a button is filled. The rule that matters is that **off is the loud
 * state**: section 4.1.1 makes a live microphone or a sending camera an
 * [OUTLINE] and a muted or stopped one [SOLID], "because being muted is the
 * thing a person needs to see at a glance".
 */
public enum class CallControlEmphasis {
    /** Translucent white fill, white icon: the facility is doing its job. */
    OUTLINE,

    /** Solid white fill, dark slashed icon: the facility is off. */
    SOLID,

    /** The end button, and nothing else on the screen. */
    DANGER,
}

/** A button of the row, ready to draw. */
public data class CallControl(
    val kind: CallControlKind,
    /** The word under the circle. Names the action, never the state. */
    val label: String,
    val emphasis: CallControlEmphasis,
    /** A disabled control is drawn at 40 percent opacity, never hidden. */
    val enabled: Boolean,
    /** An action is in flight (the mid-call camera acquire): a spinner, no icon. */
    val busy: Boolean,
    /** What TalkBack reads: the action and the state it will produce. */
    val spokenLabel: String,
)

/**
 * The buttons of the row, left to right. The only difference a group call makes
 * is the overflow entry; every call has all four of the others.
 */
public fun callControlKinds(group: Boolean): List<CallControlKind> = buildList {
    add(CallControlKind.SPEAKER)
    add(CallControlKind.MICROPHONE)
    add(CallControlKind.CAMERA)
    if (group) add(CallControlKind.MORE)
    add(CallControlKind.END)
}

/**
 * The whole row, drawn from the live call state.
 *
 * @param ringing the call is still `Calling...`. Section 4.1's state table gives
 *   that row only one active control: "only End is active". The rest stay on the
 *   screen at 40 percent, so the user sees where mute will be.
 */
@Suppress("LongParameterList")
public fun callControlRow(
    group: Boolean,
    micOn: Boolean,
    cameraOn: Boolean,
    speakerOn: Boolean,
    cameraBusy: Boolean = false,
    ringing: Boolean = false,
): List<CallControl> = callControlKinds(group).map { kind ->
    when (kind) {
        CallControlKind.SPEAKER -> CallControl(
            kind = kind,
            label = "Speaker",
            // The deviation from the default route is the loud state, the same
            // way a muted microphone is: speaker-on is what a person needs to
            // see at a glance, because it is what everyone nearby can hear.
            emphasis = if (speakerOn) CallControlEmphasis.SOLID else CallControlEmphasis.OUTLINE,
            enabled = !ringing,
            busy = false,
            spokenLabel = if (speakerOn) "Turn off speaker" else "Turn on speaker",
        )
        CallControlKind.MICROPHONE -> CallControl(
            kind = kind,
            label = "Mute",
            emphasis = if (micOn) CallControlEmphasis.OUTLINE else CallControlEmphasis.SOLID,
            enabled = !ringing,
            busy = false,
            spokenLabel = if (micOn) "Mute microphone" else "Unmute microphone",
        )
        CallControlKind.CAMERA -> CallControl(
            kind = kind,
            label = "Camera",
            emphasis = if (cameraOn) CallControlEmphasis.OUTLINE else CallControlEmphasis.SOLID,
            enabled = !ringing && !cameraBusy,
            busy = cameraBusy,
            spokenLabel = when {
                cameraBusy -> "Starting camera"
                cameraOn -> "Stop camera"
                else -> "Start camera"
            },
        )
        CallControlKind.MORE -> CallControl(
            kind = kind,
            label = "More",
            emphasis = CallControlEmphasis.OUTLINE,
            enabled = !ringing,
            busy = false,
            spokenLabel = "More controls",
        )
        CallControlKind.END -> CallControl(
            kind = kind,
            label = "End",
            emphasis = CallControlEmphasis.DANGER,
            // Always live. Hanging up is the one thing a user must be able to do
            // in every state of a call, the ringing one above all.
            enabled = true,
            busy = false,
            spokenLabel = "End call",
        )
    }
}

// ---- Fitting the row to the screen ------------------------------------------

/** Section 4.1: a round control is 64 dp across, the end button 72. */
public const val CALL_CONTROL_DIAMETER_DP: Float = 64f
public const val CALL_CONTROL_END_DIAMETER_DP: Float = 72f
/** The gap between columns; the end button is set apart by one more. */
public const val CALL_CONTROL_GAP_DP: Float = 12f
/** The bar's own horizontal padding, each side. */
public const val CALL_CONTROL_BAR_PADDING_DP: Float = 16f
/** Never smaller than the platform's minimum touch target. */
public const val CALL_CONTROL_MIN_DIAMETER_DP: Float = 48f

/** How wide each column of the bar is and how large its circles are drawn. */
public data class CallControlBarFit(
    /** Width of every column; the row is `count` of these plus the gaps and padding. */
    val columnDp: Float,
    val diameterDp: Float,
    val endDiameterDp: Float,
)

/**
 * Size the control row so that every control, the end button included, is on
 * the screen.
 *
 * The row used to give every column the end button's width and never asked
 * how wide the screen was: five columns of 72 dp, five gaps and the padding
 * came to 452 dp, and on a 411 dp phone the red End button - the one control
 * that must never be missing - was clipped past the right edge of a group
 * call. At the spec sizes five controls do not fit a 411 dp screen at all, so
 * when the room is short the circles shrink together, keeping their ratio and
 * never going under the minimum touch target; when there is room they stay
 * at the spec sizes and the row wraps its content as before.
 */
public fun fitCallControlBar(availableWidthDp: Float, count: Int): CallControlBarFit {
    if (count <= 0) {
        return CallControlBarFit(0f, CALL_CONTROL_DIAMETER_DP, CALL_CONTROL_END_DIAMETER_DP)
    }
    val room = availableWidthDp - 2f * CALL_CONTROL_BAR_PADDING_DP - count * CALL_CONTROL_GAP_DP
    val column = (room / count).coerceAtMost(CALL_CONTROL_END_DIAMETER_DP)
    val scale = (column / CALL_CONTROL_END_DIAMETER_DP).coerceIn(0f, 1f)
    val end = (CALL_CONTROL_END_DIAMETER_DP * scale).coerceAtLeast(CALL_CONTROL_MIN_DIAMETER_DP)
    val normal = (CALL_CONTROL_DIAMETER_DP * scale).coerceAtLeast(CALL_CONTROL_MIN_DIAMETER_DP)
    return CallControlBarFit(columnDp = column.coerceAtLeast(end), diameterDp = normal, endDiameterDp = end)
}
