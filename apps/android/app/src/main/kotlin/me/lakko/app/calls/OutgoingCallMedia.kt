// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * What kind of call the caller asked for, and what that means for the media the
 * offer actually advertises — `docs/ux/calls.md` section 3.
 *
 * ## The defect this closes
 *
 * The chat header has two buttons, and both produced the same call. Tapping the
 * phone (voice) button prepared the session and opened the pre-join with the
 * **camera toggle hard-coded on**, so Join dialed `video = true`, the offer
 * carried a video media flag, and the callee's ring read `Video call` for a call
 * the caller had asked to make with the phone button. Section 1 of the spec is
 * explicit that "the word on the callee's screen matches what the caller chose,
 * always"; it did not.
 *
 * The chosen kind was in fact discarded at the very first step:
 * `ChatViewModel.startCall` took `audio` and `video` and threw both away
 * (`val ignored = audio to video`) before asking the host to prepare a call,
 * because the host had nowhere to put them. [RequestedCallKind] is that place.
 *
 * ## Why a kind rather than two booleans
 *
 * A *request* and the *media finally offered* are different facts, and conflating
 * them is what made the bug invisible. The kind is what the caller asked for; it
 * decides one thing only — which surface the button opens
 * ([callEntrySurface]). What is offered is decided later: for a voice call by
 * the kind itself ([immediateDialMedia]), and for a video call by the two round
 * buttons the user is looking at when they press Call ([offeredCallMedia]).
 *
 * The rewrite of `docs/ux/calls.md` section 3 moved that second decision. It
 * used to be made on a pre-join card shown before EVERY call, whose camera
 * toggle is what this file was written to fix; that card is gone, and with it
 * `prejoinCameraDefault`, whose whole job was to open it correctly. The kind now
 * reaches [callEntrySurface] instead.
 *
 * Pure and Android-free, so both rules are unit-tested on the host JVM.
 */

/** The call kind the caller chose in the chat header. */
public enum class RequestedCallKind {
    /** The phone button: the call is dialled immediately (section 3.1). */
    VOICE,

    /** The video button: the camera check opens first (section 3.2). */
    VIDEO,
}

/** The media an offer advertises: what the pre-join toggles were set to at Join. */
public data class CallMediaChoice(val audio: Boolean, val video: Boolean)

/** The kind behind a call button, whose only input is whether it was the video one. */
public fun requestedCallKind(video: Boolean): RequestedCallKind =
    if (video) RequestedCallKind.VIDEO else RequestedCallKind.VOICE

/**
 * The media flags to dial with, read off the camera check's two round buttons
 * at the moment Call is pressed.
 *
 * Deliberately a plain projection of the two buttons rather than anything
 * derived from the requested kind: after the check, the kind has had its say.
 * A microphone switched off still dials with an audio section — the track is
 * added muted, so the user can unmute mid-call without a renegotiation — which
 * is why [CallMediaChoice.audio] carries the button and the offer's audio flag
 * follows it rather than being forced true.
 */
public fun offeredCallMedia(microphoneOn: Boolean, cameraOn: Boolean): CallMediaChoice =
    CallMediaChoice(audio = microphoneOn, video = cameraOn)

/** The word for a call of this kind, as the callee's ring and the pre-join say it. */
public fun requestedCallKindLabel(kind: RequestedCallKind): String = when (kind) {
    RequestedCallKind.VOICE -> "Voice call"
    RequestedCallKind.VIDEO -> "Video call"
}
