// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings

/**
 * What the call screen puts on the stage — `docs/ux/calls.md` section 4.
 *
 * ## The defect this closes
 *
 * There was one layout for every call: a scrolling column of 16:9 tiles with a
 * bordered card per participant. A 1x1 voice call was therefore a single empty
 * tile with one letter in it, floating on a page of dark background, with a
 * phase pill hovering over it repeating what the status line already said.
 *
 * Section 4 gives the two 1x1 shapes their own names. "A voice call is a
 * portrait: the other person's avatar large and centred in the upper half,
 * their name under it in the largest type on the screen." "A video call fills
 * the screen with the other person, and the self-view is a small rounded
 * rectangle in a corner." A group call has tiles, laid out by
 * `GroupCallLayout.kt` (`docs/ux/group-call-grid.md`).
 *
 * Pure and Android-free, so the choice is unit-tested on the host JVM.
 */

/** What the call screen draws behind the status line and the controls. */
public enum class CallStage {
    /** One other person, no video from them: the portrait of section 4. */
    PORTRAIT,

    /** One other person, sending video: their video fills the screen. */
    REMOTE_VIDEO,

    /**
     * This device has started its camera and the other person has not.
     *
     * `docs/ux/calls.md` section 4.1: pressing the camera button on a voice
     * call "starts the camera, tells the other side, and the screen becomes the
     * video layout for both", and the other person "is not forced into video".
     * So the caller who pressed it has a video layout with only one picture in
     * it — their own — and it takes the main surface rather than sitting in a
     * corner over an empty portrait. When the other person presses their own
     * button the stage becomes [REMOTE_VIDEO] and this picture moves into the
     * self-view.
     */
    LOCAL_VIDEO,

    /**
     * One other person, sharing their screen (ADR-0083,
     * `docs/ux/screen-share.md` section 5.1): the share is the large
     * picture, uncropped; their camera, or their initials, is a small tile;
     * this device's own camera keeps the self-view. A voice call takes this
     * video layout too while the share is live.
     */
    SCREEN_SHARE,

    /** A group call: its tiles, in the grid or the focus layout
     *  ([groupLayoutKind]). */
    GRID,
}

/**
 * The stage for a roster of [remoteParticipants] others, [remoteVideoPresent]
 * saying whether the sole other person is sending video and
 * [localVideoPresent] whether this device is.
 *
 * The other person's picture outranks this device's own: once both cameras are
 * on there is only one main surface and it belongs to the person being spoken
 * to, with this device's picture in the self-view.
 *
 * [groupCall] is what KIND of call this is, not how many people are in it right
 * now, and it decides before anything else. A group call whose second
 * participant has not joined yet still has a roster and an invite to offer,
 * and painting it as a 1x1 portrait would take both away until somebody else
 * arrived — so a group call keeps its tiles throughout, camera or no camera.
 */
public fun callStage(
    remoteParticipants: Int,
    remoteVideoPresent: Boolean,
    localVideoPresent: Boolean = false,
    groupCall: Boolean = false,
    /** The other person is sharing their screen (announced; its picture may
     *  not have arrived yet). A group call shows a share as a tile instead. */
    screenSharePresent: Boolean = false,
): CallStage = when {
    groupCall -> CallStage.GRID
    remoteParticipants > 1 -> CallStage.GRID
    screenSharePresent -> CallStage.SCREEN_SHARE
    remoteVideoPresent -> CallStage.REMOTE_VIDEO
    localVideoPresent -> CallStage.LOCAL_VIDEO
    else -> CallStage.PORTRAIT
}

/**
 * Whether the small rounded self-view rectangle is drawn.
 *
 * Only when this device is sending AND somebody else's picture already owns the
 * main surface. Two cases fall out of that, and both are section 4.1's words:
 *
 *  - The person who has not turned their camera on sees "the incoming picture
 *    with their self-view absent" — they are sending nothing, so there is
 *    nothing to put in the rectangle.
 *  - The person who has turned it on while the other has not is on
 *    [CallStage.LOCAL_VIDEO], where their picture IS the main surface; a
 *    self-view as well would be the same camera twice on one screen.
 */
public fun showsSelfViewPip(stage: CallStage, localVideoPresent: Boolean): Boolean =
    localVideoPresent && (stage == CallStage.REMOTE_VIDEO || stage == CallStage.SCREEN_SHARE)

/**
 * What the call screen is titled — `docs/ux/calls.md` section 4's status line.
 *
 * ## The defect this closes
 *
 * The title was whatever the first remote participant's tile was labelled, with
 * no notion of a group. On a live three-person call in a group named "Trio",
 * the screen was titled `01a0a985` — the peer's SFU account pseudonym, which is
 * what the roster falls back to when it cannot resolve a contact. So a failure
 * to name ONE participant renamed the whole call, and the group's own name,
 * which ADR-0051 makes a shared end-to-end-encrypted fact every member holds,
 * appeared nowhere.
 *
 * ## The rule
 *
 * A group call is titled by the group, full stop — **never** by a participant,
 * however that participant happens to be labelled. That is the part worth
 * pinning: a participant label is a per-person fallback chain that can and does
 * end in an opaque identifier, and a title derived from it inherits every one
 * of those failures. A group with no name says so plainly instead.
 *
 * A 1x1 call is titled by the other person, because there the person IS the
 * call, and falls back to a neutral word rather than to anything opaque.
 */
public fun callScreenTitle(
    groupName: String?,
    peerName: String?,
    groupCall: Boolean,
    strings: Strings = Localization.strings.value,
): String {
    val group = groupName?.trim()?.takeIf { it.isNotEmpty() }
    val peer = peerName?.trim()?.takeIf { it.isNotEmpty() }
    return if (groupCall) group ?: strings["call.label.group"] else peer ?: strings["chat.call.voice"]
}

/** How long the controls stay up before fading on a connected video call. */
public const val CALL_CONTROLS_AUTO_HIDE_MS: Long = 5_000L

/**
 * Whether the controls may fade away after [CALL_CONTROLS_AUTO_HIDE_MS].
 *
 * Section 4: "The controls fade out after 5 seconds of a connected video call
 * and come back on a tap; on a voice call they never hide." Section 4.2 gives
 * the reason the rule is not general — on a voice call there is "nothing to look
 * at underneath, so hiding them only makes the user tap".
 *
 * A grid call is treated as a voice call here for the same reason: what is
 * underneath is a roster, not a picture of a person, and a group call is where
 * losing the mute button costs most.
 *
 * `alone` extends that to the case the rule could not reach when it was
 * written. A call used to be entered from a conversation, where somebody else
 * was already there; a call started from the Calls screen begins with one
 * person in it, and the only picture is this device's own. Found live on iOS
 * on 2026-09-23 and fixed on both: five seconds after starting a call the row
 * faded, leaving the person who had just created it looking at their own
 * camera with no way to invite anyone and no way to hang up. A tap brings it
 * back, but a screen whose only remaining affordance is an undiscoverable one
 * is a screen with no exit.
 */
public fun controlsMayAutoHide(
    stage: CallStage,
    connected: Boolean,
    alone: Boolean = false,
): Boolean =
    connected &&
        !alone &&
        (stage == CallStage.REMOTE_VIDEO || stage == CallStage.LOCAL_VIDEO || stage == CallStage.SCREEN_SHARE)
