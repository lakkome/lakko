// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What pressing a call button does, before any call exists
 * (`docs/ux/calls.md` §3).
 *
 * ## What this replaces
 *
 * A pre-join screen that opened for **both** buttons: a card headed "Ready to
 * join?", a bordered box with a camera glyph in it where a picture was not, and
 * two toggles under it. §3.1 is blunt about why that is wrong for a voice call —
 * "asking a question before a voice call is the thing that made the old screen
 * feel like a settings page" — and §4.1 is blunt about the toggles: "a switch
 * says 'this is a preference you are configuring'. A call needs a button you
 * press."
 *
 * So the pre-call screen survives only for a **video** call, only as §3.2's
 * camera check (a live self-view filling the screen, three round buttons over
 * it), and a voice call goes straight to `Calling...`.
 *
 * ## Why the camera is probed before the check opens, and not inside it
 *
 * §3.2's last sentence: "If the device has no camera, the video button starts a
 * voice call and says so in one line." A check that opened and *then* discovered
 * it had nothing to show would put the empty box back on the screen — the exact
 * thing this change removes — and would make the user dismiss a screen that had
 * told them nothing. So the caller acquires the camera first and
 * {@link planCallStart} is told the answer.
 *
 * ## Why this is a module
 *
 * `vitest` has no jsdom and no Solid transform here, so a rule written inside
 * `routes/Chat.tsx` — 300 KB of one component — is a rule no test can reach.
 * The rule that decides whether a person is asked a question before their phone
 * call is worth reaching.
 */

import { mediaForToggles, type CallMediaChoice, type CallStartKind } from "./call-media-choice";

/** The one line shown when the video button could not open a camera (§3.2). */
export const CAMERA_UNAVAILABLE_NOTICE =
  "No camera available, so this is a voice call.";

/** What a press of a call button leads to. */
export type CallStartPlan =
  /** Place the call now. `notice` is the one line §3.2 owes the user when the
   *  call they asked for is not the call they are getting. */
  | { readonly kind: "dial"; readonly media: CallMediaChoice; readonly notice: string | null }
  /** Open §3.2's camera check first. */
  | { readonly kind: "camera-check" };

/**
 * What the chat header's two buttons do.
 *
 * The phone button dials, always and immediately (§3.1). The video button opens
 * the camera check when there is a camera to check, and otherwise places the
 * voice call it can actually place, saying so.
 */
export function planCallStart(kind: CallStartKind, cameraAvailable: boolean): CallStartPlan {
  if (kind === "audio") {
    return { kind: "dial", media: { audio: true, video: false }, notice: null };
  }
  if (!cameraAvailable) {
    return {
      kind: "dial",
      media: { audio: true, video: false },
      notice: CAMERA_UNAVAILABLE_NOTICE,
    };
  }
  return { kind: "camera-check" };
}

/**
 * What the camera check's Call button places.
 *
 * It reads the toggles, not the button that opened the screen: once a screen
 * has offered the user a camera switch, the switch is the answer, and §1
 * requires the word on the callee's screen to match. Turning the camera off in
 * the check and calling anyway is a voice call, and the callee is told so.
 *
 * The microphone state rides alongside rather than inside `media.audio`: on the
 * web `audio: false` would mean no audio transceiver at all, so a call joined
 * muted could never be unmuted without renegotiating. Joining muted is not
 * joining with no microphone. This is the same split `call-media-choice.ts`
 * documents.
 */
export function cameraCheckResult(toggles: {
  readonly micOn: boolean;
  readonly camOn: boolean;
}): { readonly media: CallMediaChoice; readonly micOn: boolean } {
  return { media: mediaForToggles(toggles), micOn: toggles.micOn };
}

/** A round button on the camera check. Same two states as the in-call row
 *  (`calls/call-controls.ts`), because §4.1 says the check "uses the same round
 *  buttons, not switches". */
export interface CameraCheckControl {
  readonly label: string;
  readonly accessibleName: string;
  readonly icon: "mic" | "micOff" | "video" | "videoOff";
  /** `outline` when the device is live, `solid` when it is off — off is the
   *  loud state (§4.1.1). */
  readonly tone: "outline" | "solid";
  readonly pressed: boolean;
}

/**
 * The check's two toggles, which sit either side of the Call button (§3.2).
 *
 * Their labels are the in-call labels, so the two screens do not use two
 * vocabularies for one microphone.
 */
export function cameraCheckControls(toggles: {
  readonly micOn: boolean;
  readonly camOn: boolean;
}): { readonly mic: CameraCheckControl; readonly camera: CameraCheckControl } {
  return {
    mic: {
      label: "Mute",
      accessibleName: toggles.micOn ? "Mute microphone" : "Unmute microphone",
      icon: toggles.micOn ? "mic" : "micOff",
      tone: toggles.micOn ? "outline" : "solid",
      pressed: !toggles.micOn,
    },
    camera: {
      label: "Camera",
      accessibleName: toggles.camOn ? "Turn off camera" : "Turn on camera",
      icon: toggles.camOn ? "video" : "videoOff",
      tone: toggles.camOn ? "outline" : "solid",
      pressed: !toggles.camOn,
    },
  };
}

/**
 * The word on the camera check's big button.
 *
 * Constant on purpose. §3.2: "Turning the camera off there does not leave a
 * hole: the preview becomes the callee's avatar on the call background, and the
 * button still reads **Call**." A button that became "Call without video" would
 * turn a two-second look in the mirror back into a decision.
 */
export function cameraCheckCallLabel(): string {
  return "Call";
}

/**
 * What a screen reader is told the big button does.
 *
 * Here the state *is* worth saying, because it is the one thing the sighted
 * user reads off the preview and a screen-reader user cannot.
 */
export function cameraCheckCallAccessibleName(
  peerName: string | null,
  camOn: boolean,
): string {
  const kind = camOn ? "video call" : "voice call";
  const name = peerName?.trim() ?? "";
  return name === "" ? `Start ${kind}` : `Start ${kind} with ${name}`;
}
