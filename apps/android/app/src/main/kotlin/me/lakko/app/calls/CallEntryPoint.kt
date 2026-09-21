// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Which surface a call button opens — `docs/ux/calls.md` sections 3.1 and 3.2.
 *
 * ## The defect this closes
 *
 * Both header buttons opened the same preparation screen: a dark card titled
 * "Join call" holding an empty 16:9 "Camera preview" box, two settings-style
 * toggle rows and a Join button. For a **voice** call that screen asks a
 * question nobody wants before dialling — no other phone app does it, and the
 * empty preview box makes a voice call look like a misconfigured video one.
 * Section 3.1 is explicit: "Tapping the phone button places the call
 * immediately. No preparation screen."
 *
 * A **video** call used to stop at a camera check — a full-screen live
 * self-view with a Call button under it — on the ground that "a person about to
 * be seen deserves to know what they look like". That check was removed on
 * 2026-09-21 and section 3.2 now reads the other way: one press of a call
 * button is one call, for both kinds. The reason behind the check is not
 * abandoned, it is already met elsewhere — the self-view sits on the calling
 * screen, so the caller sees themselves while it rings, the same look a moment
 * later and without a second press.
 *
 * ## Why the camera availability is still an input here
 *
 * With the check gone the surface no longer varies, but the MEDIA does: a
 * device with no usable camera places a **voice** call and says so in one line
 * ([NO_CAMERA_VOICE_FALLBACK_NOTICE]) rather than dialling a video call it
 * cannot make — the same posture the camera-permission fallback already takes
 * in [me.lakko.app.ui.call.CallPermissionGate].
 *
 * Pure and Android-free, so every branch is unit-tested on the host JVM. The
 * caller supplies the camera fact; on Android that is
 * [CameraVideoCapture.hasUsableCamera].
 */

/**
 * What the call button opens before any media is offered.
 *
 * One value, and that is the decision rather than an oversight: the enum is
 * kept so the call sites still ask the question in one place, and so a future
 * surface (a group-call lobby, say) has somewhere to be added without every
 * caller being rewritten. `CAMERA_CHECK` was the other value until
 * 2026-09-21.
 */
public enum class CallEntrySurface {
    /** Nothing: dial now and open the call screen on `Calling...`. Both kinds. */
    DIAL_IMMEDIATELY,
}

/**
 * The one line shown when the video button places a voice call because the
 * device has no camera. Deliberately one sentence: what happened, in the words
 * of what the user asked for.
 */
public const val NO_CAMERA_VOICE_FALLBACK_NOTICE: String =
    "This device has no camera, so Lakko is placing a voice call."

/**
 * The surface [kind] opens on a device where a camera is or is not usable.
 *
 * Both parameters are unused and both are kept: the signature is what the call
 * sites already ask, and a rule that answers the same for every input is worth
 * stating once here rather than being inlined as `true` at each of them, where
 * the next surface would have to be threaded back through by hand.
 */
@Suppress("UNUSED_PARAMETER")
public fun callEntrySurface(kind: RequestedCallKind, hasCamera: Boolean): CallEntrySurface =
    CallEntrySurface.DIAL_IMMEDIATELY

/**
 * The media a call of this [kind] dials with when it skips the camera check.
 *
 * A voice call carries no video; a video call carries the camera when the
 * device has one, and none when it does not. The microphone is always offered
 * — the call screen
 * has a Mute button, and a call dialled with no audio section could never
 * unmute without a renegotiation.
 */
public fun immediateDialMedia(kind: RequestedCallKind, hasCamera: Boolean): CallMediaChoice =
    CallMediaChoice(audio = true, video = kind == RequestedCallKind.VIDEO && hasCamera)

/**
 * The notice to show alongside the call, or `null` when there is nothing to
 * say. Only the no-camera fallback speaks: a voice call behaving like a voice
 * call needs no explanation.
 */
public fun callEntryNotice(kind: RequestedCallKind, hasCamera: Boolean): String? =
    if (kind == RequestedCallKind.VIDEO && !hasCamera) NO_CAMERA_VOICE_FALLBACK_NOTICE else null
