// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.UiText

/**
 * The call screen's control row — `docs/ux/calls.md` sections 4.1 and 4.1.1.
 *
 * ## The defect this closes
 *
 * Section 4.1 names the faults of the old row:
 *
 *  - **The wrong kind of control.** The pre-call screen configured the call with
 *    switches in a settings list. "A switch says 'this is a preference you are
 *    configuring'. A call needs a button you press."
 *  - **Real actions behind an ellipsis menu.** Switching camera lived in a
 *    dropdown — "something a person does mid-sentence with one thumb; a menu
 *    makes it two taps and a reading task".
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
 * The row is the same for both kinds of one-to-one call — Microphone, Camera,
 * Settings, End — because **the camera button is on a voice call too, and it
 * turns the call into a video one**. Section 4.1: "People start a call with their voice and
 * decide to show something a minute later; every messenger allows that, and
 * taking it away to keep the row short would be a feature lost to tidiness."
 *
 * That is why the row takes no "is this a video call" input. There is no such
 * thing on a live call: there is a camera that is sending or not, and the
 * button that changes which. Deriving the row from a call *kind* is what would
 * strand a voice call with no way to start showing something.
 *
 * A group call adds one entry, **Participants**: who is in the call, and, on a
 * call that can hand out a link to itself (`docs/ux/call-links.md` section 3),
 * the two ways to invite someone; what it does is [participantsControl]. So a
 * group row is Microphone, Camera, Participants, Settings, End, always in ONE
 * row, on every screen size.
 *
 * **There is no Speaker control**, on any call. Where the sound comes out is
 * a rule ([callPlaysOnLoudspeaker], [callOutputTarget]) and, for the person
 * who wants something else, a choice in the **Settings** sheet, which also
 * holds the microphone and the camera (`CallSettings.kt`).
 *
 * There is no overflow menu and no chat in a call: the chat control sent
 * nothing anywhere, raise hand and share screen changed a flag nothing read,
 * and a control that does nothing is worse than none (removed 2026-10-02).
 * Flip camera is not in the row: it is on the self-view rectangle, "where the
 * thing it changes is visible", and every camera is in the Settings sheet.
 */

/** One button of the row. Flip camera is not here; see the file header. */
public enum class CallControlKind {
    /** Mute and unmute the microphone. */
    MICROPHONE,

    /** Start and stop sending the camera. */
    CAMERA,

    /** Group calls only: who is in the call, and how to invite someone. */
    PARTICIPANTS,

    /** Which microphone, camera and speaker the call uses. */
    SETTINGS,

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
 * What the Participants control does when pressed.
 *
 * One control where there were two (Participants and Invite), because on a
 * phone every circle in the row costs every other one its size.
 */
public enum class ParticipantsControl {
    /** No control: there is nobody to list and no link to offer. */
    ABSENT,

    /** Opens the list of who is in the call, directly: a menu holding one
     *  item would be a second step for nothing. */
    LIST,

    /** Opens the menu of [participantsMenuItems]. */
    MENU,
}

/**
 * What Participants is, for the call on screen.
 *
 * @param group this is a group or a link call; a one-to-one has no such
 *   control.
 * @param canInvite the call can hand out a link to itself: one a `/c/` link
 *   names, on the device that hosts it ([showsCallInvite]). A call placed
 *   inside a conversation is reached through that conversation, and a link
 *   minted over its id would lead a stranger to a call they cannot be
 *   admitted to.
 * @param othersPresent at least one other person is in the call.
 */
public fun participantsControl(group: Boolean, canInvite: Boolean, othersPresent: Boolean): ParticipantsControl =
    when {
        !group -> ParticipantsControl.ABSENT
        canInvite -> ParticipantsControl.MENU
        othersPresent -> ParticipantsControl.LIST
        else -> ParticipantsControl.ABSENT
    }

/** One item of the menu Participants opens. */
public enum class ParticipantsMenuItem {
    /** The list of who is in the call. */
    PARTICIPANTS,

    /** Put the link to this call on the clipboard. */
    COPY_LINK,

    /** Send the invitation into conversations the person picks. */
    INVITE_CONTACTS,
}

/**
 * The items of the menu, in order.
 *
 * `Participants` only when someone else is in the call: alone, the list would
 * hold one name, the person's own.
 *
 * `Invite contacts` is there for every account, with or without
 * conversations: an item that comes and goes with the chat list reads as a
 * missing feature (found live on web, 2026-10-02), so the list it opens says
 * so itself when it is empty. Only a [guest] account
 * (`auth/GuestAccount.kt`), which never has conversations, gets the link
 * alone.
 */
public fun participantsMenuItems(othersPresent: Boolean, guest: Boolean): List<ParticipantsMenuItem> =
    buildList {
        if (othersPresent) add(ParticipantsMenuItem.PARTICIPANTS)
        add(ParticipantsMenuItem.COPY_LINK)
        if (!guest) add(ParticipantsMenuItem.INVITE_CONTACTS)
    }

/**
 * The buttons of the row, left to right: Microphone, Camera, Settings and End
 * on every call, and [participants] before Settings on a group call. End
 * keeps the far side of the row to itself.
 */
public fun callControlKinds(participants: Boolean = false): List<CallControlKind> = buildList {
    add(CallControlKind.MICROPHONE)
    add(CallControlKind.CAMERA)
    if (participants) add(CallControlKind.PARTICIPANTS)
    add(CallControlKind.SETTINGS)
    add(CallControlKind.END)
}

/**
 * The whole row, drawn from the live call state.
 *
 * @param ringing the call is still `Calling...`. Section 4.1's state table gives
 *   that row only one active control: "only End is active". The rest stay on the
 *   screen at 40 percent, so the user sees where mute will be.
 * @param participants what the Participants control does; absent from the row
 *   when [ParticipantsControl.ABSENT].
 * @param participantsOpen the participant list is open, which the control's
 *   spoken label follows when it opens the list directly.
 */
@Suppress("LongParameterList")
public fun callControlRow(
    micOn: Boolean,
    cameraOn: Boolean,
    cameraBusy: Boolean = false,
    ringing: Boolean = false,
    participants: ParticipantsControl = ParticipantsControl.ABSENT,
    participantsOpen: Boolean = false,
): List<CallControl> = callControlKinds(
    participants = participants != ParticipantsControl.ABSENT,
).map { kind ->
    when (kind) {
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
            // Not a toggle: it opens a list or a menu, and is never left
            // switched on.
            emphasis = CallControlEmphasis.OUTLINE,
            // Usable in every state of the call, the first second included:
            // who is here, and the link to bring someone, are not things that
            // wait for media.
            enabled = true,
            busy = false,
            spokenLabel = UiText(
                when {
                    participants == ParticipantsControl.MENU -> "call.control.participants_menu_a11y"
                    participantsOpen -> "call.control.participants_hide_a11y"
                    else -> "call.control.participants_show_a11y"
                },
            ),
        )
        CallControlKind.SETTINGS -> CallControl(
            kind = kind,
            label = UiText("call.control.settings"),
            // Not a toggle: it opens a sheet.
            emphasis = CallControlEmphasis.OUTLINE,
            // Active from the first second of a call: which microphone the
            // call uses is worth choosing before anyone answers.
            enabled = true,
            busy = false,
            spokenLabel = UiText("call.settings.title"),
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
