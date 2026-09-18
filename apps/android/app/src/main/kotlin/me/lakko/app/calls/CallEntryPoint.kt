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
 * A **video** call still shows something first, because "a person about to be
 * seen deserves to know what they look like" — but a camera check (section
 * 3.2), which is the live self-view filling the screen, not a settings card.
 *
 * ## Why the camera availability is an input here
 *
 * A camera check with no camera is the empty box again, with a Call button that
 * would place a video call the device cannot make. So a device with no usable
 * camera skips the check and places a **voice** call, and says so in one line
 * ([NO_CAMERA_VOICE_FALLBACK_NOTICE]) rather than silently degrading — the same
 * posture the camera-permission fallback already takes in
 * [me.lakko.app.ui.call.CallPermissionGate].
 *
 * Pure and Android-free, so every branch is unit-tested on the host JVM. The
 * caller supplies the camera fact; on Android that is
 * [CameraVideoCapture.hasUsableCamera].
 */

/** What the call button opens before any media is offered. */
public enum class CallEntrySurface {
    /**
     * Nothing: dial now and open the call screen on `Calling...`. A voice call,
     * and a video call on a device with no camera.
     */
    DIAL_IMMEDIATELY,

    /**
     * The camera check of section 3.2 — the live self-view, the callee's name,
     * and a Call button. Only a video call on a device that has a camera.
     */
    CAMERA_CHECK,
}

/**
 * The one line shown when the video button places a voice call because the
 * device has no camera. Deliberately one sentence: what happened, in the words
 * of what the user asked for.
 */
public const val NO_CAMERA_VOICE_FALLBACK_NOTICE: String =
    "This device has no camera, so Lakko is placing a voice call."

/** The surface [kind] opens on a device where a camera is or is not usable. */
public fun callEntrySurface(kind: RequestedCallKind, hasCamera: Boolean): CallEntrySurface = when {
    kind == RequestedCallKind.VOICE -> CallEntrySurface.DIAL_IMMEDIATELY
    !hasCamera -> CallEntrySurface.DIAL_IMMEDIATELY
    else -> CallEntrySurface.CAMERA_CHECK
}

/**
 * The media a call of this [kind] dials with when it skips the camera check.
 *
 * A voice call carries no video; a video call that reached here has no camera,
 * so it carries none either. The microphone is always offered — the call screen
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
