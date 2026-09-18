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
 * decides one thing only — how the pre-join opens ([prejoinCameraDefault]). What
 * is offered is decided later, by the toggles the user is looking at when they
 * press Join ([offeredCallMedia]), which is what the spec means by "the toggles
 * there change the call that is actually placed". A voice call whose camera the
 * user switches on in the pre-join is a video call, and correctly says so.
 *
 * Pure and Android-free, so both rules are unit-tested on the host JVM.
 */

/** The call kind the caller chose in the chat header. */
public enum class RequestedCallKind {
    /** The phone button: the pre-join opens with the camera off. */
    VOICE,

    /** The video button: the pre-join opens with the camera on. */
    VIDEO,
}

/** The media an offer advertises: what the pre-join toggles were set to at Join. */
public data class CallMediaChoice(val audio: Boolean, val video: Boolean)

/** The kind behind a call button, whose only input is whether it was the video one. */
public fun requestedCallKind(video: Boolean): RequestedCallKind =
    if (video) RequestedCallKind.VIDEO else RequestedCallKind.VOICE

/**
 * How the pre-join's camera toggle opens for [kind].
 *
 * This is the whole of the fix's first half: a voice call never turns the camera
 * on by itself. It is still only a *default* — the user may switch it on, and
 * then they have chosen a video call.
 */
public fun prejoinCameraDefault(kind: RequestedCallKind): Boolean = kind == RequestedCallKind.VIDEO

/**
 * The media flags to dial with, read off the pre-join toggles at the moment Join
 * is pressed.
 *
 * Deliberately a plain projection of the two switches rather than anything
 * derived from the requested kind: after the pre-join, the kind has had its say.
 * A microphone switched off still dials with an audio section — the track is
 * added muted, so the user can unmute mid-call without a renegotiation — which
 * is why [CallMediaChoice.audio] carries the switch and the offer's audio flag
 * follows it rather than being forced true.
 */
public fun offeredCallMedia(microphoneOn: Boolean, cameraOn: Boolean): CallMediaChoice =
    CallMediaChoice(audio = microphoneOn, video = cameraOn)

/** The word for a call of this kind, as the callee's ring and the pre-join say it. */
public fun requestedCallKindLabel(kind: RequestedCallKind): String = when (kind) {
    RequestedCallKind.VOICE -> "Voice call"
    RequestedCallKind.VIDEO -> "Video call"
}
