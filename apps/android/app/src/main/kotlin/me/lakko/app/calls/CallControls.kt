// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.UiText

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
 * A group call adds one entry, the list of who is in it, and a call that can
 * hand out a link to itself adds `Invite` (`docs/ux/call-links.md` section 3):
 * Speaker, Mute, Camera, Participants, Invite, End, always in ONE row. There is
 * no overflow menu and no chat in a call: the chat control sent nothing
 * anywhere, raise hand and share screen changed a flag nothing read, and a
 * control that does nothing is worse than none (removed 2026-10-02). Flip
 * camera is deliberately absent from every set: it belongs on the self-view
 * rectangle, "where the thing it changes is visible".
 */

/** One button of the row. Flip camera is not here; see the file header. */
public enum class CallControlKind {
    /** Route the call audio to the loudspeaker instead of the earpiece. */
    SPEAKER,

    /** Mute and unmute the microphone. */
    MICROPHONE,

    /** Start and stop sending the camera. */
    CAMERA,

    /** Group calls only: open and close the list of who is in the call. */
    PARTICIPANTS,

    /** Calls that can hand out a link to themselves only: the invite menu. */
    INVITE,

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
    val label: UiText,
    val emphasis: CallControlEmphasis,
    /** A disabled control is drawn at 40 percent opacity, never hidden. */
    val enabled: Boolean,
    /** An action is in flight (the mid-call camera acquire): a spinner, no icon. */
    val busy: Boolean,
    /** What TalkBack reads: the action and the state it will produce. */
    val spokenLabel: UiText,
)

/**
 * The buttons of the row, left to right. A group call adds the participant
 * list; a call that can hand out a link to itself ([canInvite]) adds `Invite`,
 * beside the other group entry and before End, so the destructive control
 * keeps the far side of the row to itself. Every call has all four of the
 * others.
 *
 * Speaker is on every call because on this platform it always does
 * something: it moves the sound between the earpiece and the loudspeaker
 * ([CallAudioRouter]).
 */
public fun callControlKinds(group: Boolean, canInvite: Boolean = false): List<CallControlKind> = buildList {
    add(CallControlKind.SPEAKER)
    add(CallControlKind.MICROPHONE)
    add(CallControlKind.CAMERA)
    if (group) add(CallControlKind.PARTICIPANTS)
    if (canInvite) add(CallControlKind.INVITE)
    add(CallControlKind.END)
}

/**
 * The whole row, drawn from the live call state.
 *
 * @param ringing the call is still `Calling...`. Section 4.1's state table gives
 *   that row only one active control: "only End is active". The rest stay on the
 *   screen at 40 percent, so the user sees where mute will be.
 * @param participantsOpen the participant list is open (group only); the
 *   button reads pressed while it is.
 * @param canInvite this call can hand out a link to itself: one a `/c/` link
 *   names, on the device that hosts it ([showsCallInvite]). A call placed
 *   inside a conversation is reached through that conversation, so the
 *   control is absent there rather than present and broken.
 * @param inviteBusy the link is being prepared: the button shows a spinner
 *   and refuses a second press rather than paying for a second one.
 */
@Suppress("LongParameterList")
public fun callControlRow(
    group: Boolean,
    micOn: Boolean,
    cameraOn: Boolean,
    speakerOn: Boolean,
    cameraBusy: Boolean = false,
    ringing: Boolean = false,
    participantsOpen: Boolean = false,
    canInvite: Boolean = false,
    inviteBusy: Boolean = false,
): List<CallControl> = callControlKinds(group, canInvite).map { kind ->
    when (kind) {
        CallControlKind.SPEAKER -> CallControl(
            kind = kind,
            label = UiText("call.control.speaker"),
            // The deviation from the default route is the loud state, the same
            // way a muted microphone is: speaker-on is what a person needs to
            // see at a glance, because it is what everyone nearby can hear.
            emphasis = if (speakerOn) CallControlEmphasis.SOLID else CallControlEmphasis.OUTLINE,
            enabled = !ringing,
            busy = false,
            spokenLabel = UiText(if (speakerOn) "call.control.speaker_off_a11y" else "call.control.speaker_on_a11y"),
        )
        CallControlKind.MICROPHONE -> CallControl(
            kind = kind,
            label = UiText("call.control.mute"),
            emphasis = if (micOn) CallControlEmphasis.OUTLINE else CallControlEmphasis.SOLID,
            enabled = !ringing,
            busy = false,
            spokenLabel = UiText(if (micOn) "call.control.mute_a11y" else "call.control.unmute_a11y"),
        )
        CallControlKind.CAMERA -> CallControl(
            kind = kind,
            label = UiText("call.control.camera"),
            emphasis = if (cameraOn) CallControlEmphasis.OUTLINE else CallControlEmphasis.SOLID,
            enabled = !ringing && !cameraBusy,
            busy = cameraBusy,
            spokenLabel = UiText(
                when {
                    cameraBusy -> "call.self.starting_camera"
                    cameraOn -> "call.control.camera_stop_a11y"
                    else -> "call.control.camera_start_a11y"
                },
            ),
        )
        CallControlKind.PARTICIPANTS -> CallControl(
            kind = kind,
            label = UiText("call.control.participants"),
            emphasis = CallControlEmphasis.OUTLINE,
            // The list is readable in every state of the call, the first
            // second included.
            enabled = true,
            busy = false,
            spokenLabel = UiText(
                if (participantsOpen) "call.control.participants_hide_a11y" else "call.control.participants_show_a11y",
            ),
        )
        CallControlKind.INVITE -> CallControl(
            kind = kind,
            label = UiText("call.control.invite"),
            // Not a toggle: the link is offered, never left switched on.
            emphasis = CallControlEmphasis.OUTLINE,
            enabled = !inviteBusy,
            busy = inviteBusy,
            spokenLabel = UiText("call.control.invite_a11y"),
        )
        CallControlKind.END -> CallControl(
            kind = kind,
            label = UiText("call.control.end"),
            emphasis = CallControlEmphasis.DANGER,
            // Always live. Hanging up is the one thing a user must be able to do
            // in every state of a call, the ringing one above all.
            enabled = true,
            busy = false,
            spokenLabel = UiText("call.control.end_a11y"),
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

// ---- The group row ----------------------------------------------------------

/** Below either of these the group row is the compact one (dp). */
public const val GROUP_CONTROL_COMPACT_WIDTH_DP: Float = 600f
public const val GROUP_CONTROL_COMPACT_HEIGHT_DP: Float = 520f

/** How a group call's control row is drawn. */
public data class GroupControlBarFit(
    val diameterDp: Float,
    val endDiameterDp: Float,
    /** The space between circles; the end button is set apart by one more. */
    val gapDp: Float,
    /** The words under the circles. Absent on the compact row, where each
     *  circle keeps its spoken name. */
    val showLabels: Boolean,
    val paddingHorizontalDp: Float,
    val paddingVerticalDp: Float,
) {
    /** The whole bar's width for [count] controls. */
    public fun widthDp(count: Int): Float =
        if (count <= 0) 0f
        else (count - 1) * diameterDp + endDiameterDp + count * gapDp + 2f * paddingHorizontalDp
}

/**
 * Size a group call's control row (`docs/ux/calls.md` section 4.1: "the group
 * row is always one line").
 *
 * With room (600 dp wide and 520 dp tall, or more) the circles are 56 dp, End
 * 64 dp, with the words under them. On a phone, upright or on its side, the
 * words go and the circles are sized from the width and the number of
 * controls, between 38 and 52 dp, so the row fits. A row that wrapped into
 * two lines of labelled circles took half a phone screen (found live
 * 2026-10-02), and before that the End button of a five-control row was
 * clipped past the edge.
 *
 * The numbers are the web client's (`styles.css`, the group rule of
 * `.lk-call-bar`), so the three rows shrink alike on the same width. One
 * thing is this client's own: on a width so narrow that 38 dp circles still
 * do not fit, the gaps close up rather than a control leaving the screen.
 *
 * @param availableWidthDp the width the bar may take, the side margins
 *   already taken off.
 * @param screenHeightDp the height of the call screen.
 */
public fun fitGroupControlBar(availableWidthDp: Float, screenHeightDp: Float, count: Int): GroupControlBarFit {
    val compact = availableWidthDp + 2f * GROUP_CONTROL_SIDE_MARGIN_DP < GROUP_CONTROL_COMPACT_WIDTH_DP ||
        screenHeightDp < GROUP_CONTROL_COMPACT_HEIGHT_DP
    if (!compact || count <= 0) {
        return GroupControlBarFit(
            diameterDp = 56f,
            endDiameterDp = 64f,
            gapDp = 14f,
            showLabels = true,
            paddingHorizontalDp = 20f,
            paddingVerticalDp = 12f,
        )
    }
    val padding = 10f
    val endExtra = 6f
    val gap = 8f
    // What is left for the circles once the padding, the gaps and the end
    // button's extra width are taken off.
    val room = availableWidthDp - 2f * padding - endExtra
    val diameter = ((room - count * gap) / count).coerceIn(38f, 52f)
    // Too narrow even for the smallest circle: give up gap, not a control.
    val fittedGap = if (count * (diameter + gap) > room) {
        ((room - count * diameter) / count).coerceIn(0f, gap)
    } else {
        gap
    }
    return GroupControlBarFit(
        diameterDp = diameter,
        endDiameterDp = diameter + endExtra,
        gapDp = fittedGap,
        showLabels = false,
        paddingHorizontalDp = padding,
        paddingVerticalDp = 8f,
    )
}

/** The margin the bar keeps from each side of the screen. */
public const val GROUP_CONTROL_SIDE_MARGIN_DP: Float = 12f
