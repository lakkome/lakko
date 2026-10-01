// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.UiText

/**
 * One call at a time, across the three kinds of call the Android client can be
 * in. The Kotlin twin of `apps/web/src/calls/call-slot.ts`.
 *
 * The app-level call host ([AppCallHost]) holds a 1x1 call
 * ([OneToOneCallHost]), a group (SFU) call in a chat ([GroupCallHost]) and an
 * ADR-0032 call-link call ([LinkCallHost]). They share the microphone, the
 * camera and the full-screen in-call surface, so two of them live at once
 * would not be two calls. Before the group and link calls moved into the host,
 * a `/c/` link opened during a group call started a second SFU session while
 * the first kept running with no screen left to end it.
 *
 * Every entry point into a call asks [callSlotOccupant] first, and a refusal is
 * explained with [callBusyMessage] rather than silently ignored.
 */

/** The kind of call occupying the slot. */
public enum class CallKind { ONE_TO_ONE, GROUP, LINK }

/** What the user tried to do when a call was refused. */
public enum class CallVerb { START, JOIN }

/**
 * The call occupying the slot, or `null` when it is free. At most one input is
 * true in a correct host; the order only makes a bug deterministic.
 *
 * @param oneToOneInCall a 1x1 call is ringing, prepared, dialing, live or ending.
 * @param groupCallActive a group call is starting or live.
 * @param linkCallActive a call-link call is starting, waiting for admission or live.
 */
public fun callSlotOccupant(
    oneToOneInCall: Boolean,
    groupCallActive: Boolean,
    linkCallActive: Boolean,
): CallKind? = when {
    oneToOneInCall -> CallKind.ONE_TO_ONE
    groupCallActive -> CallKind.GROUP
    linkCallActive -> CallKind.LINK
    else -> null
}

/**
 * Why a new call was refused: [occupant] is the call already running, [verb]
 * what the user tried to do. Says what to do about it, because the refusal is
 * the whole outcome. One whole sentence per pair (`call.busy.*`), the same
 * keys as the web client, never a phrase slotted into a frame.
 */
public fun callBusyMessage(occupant: CallKind, verb: CallVerb): UiText {
    val kind = when (occupant) {
        CallKind.ONE_TO_ONE -> "call"
        CallKind.GROUP -> "group"
        CallKind.LINK -> "link"
    }
    val action = when (verb) {
        CallVerb.START -> "start"
        CallVerb.JOIN -> "join"
    }
    return UiText("call.busy.${kind}_$action")
}
