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
 * The table from section 4.1:
 *
 * | Call kind | The row, left to right |
 * |---|---|
 * | Voice | Speaker, Mute, End |
 * | Video | Speaker, Mute, Camera, End |
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
 * The buttons of the row, left to right, for a call of this shape.
 *
 * [videoCall] is "this call has a video dimension" rather than "the caller
 * pressed the video button": a 1x1 call that gained video mid-call must keep a
 * camera button, or the only way to stop sending would be to hang up.
 */
public fun callControlKinds(group: Boolean, videoCall: Boolean): List<CallControlKind> = when {
    group -> listOf(
        CallControlKind.SPEAKER,
        CallControlKind.MICROPHONE,
        CallControlKind.CAMERA,
        CallControlKind.MORE,
        CallControlKind.END,
    )
    videoCall -> listOf(
        CallControlKind.SPEAKER,
        CallControlKind.MICROPHONE,
        CallControlKind.CAMERA,
        CallControlKind.END,
    )
    else -> listOf(
        CallControlKind.SPEAKER,
        CallControlKind.MICROPHONE,
        CallControlKind.END,
    )
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
    videoCall: Boolean,
    micOn: Boolean,
    cameraOn: Boolean,
    speakerOn: Boolean,
    cameraBusy: Boolean = false,
    ringing: Boolean = false,
): List<CallControl> = callControlKinds(group, videoCall).map { kind ->
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
