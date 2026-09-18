// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a call actually asks for, from the button that started it through the
 * camera check's toggles (`docs/ux/calls.md` §3).
 *
 * ## The rule this exists to hold
 *
 * §1: "Say what kind of call it is. The word on the callee's screen matches
 * what the caller chose, always." So whatever the user leaves the camera
 * check's toggles at is what the **offer** carries: a toggle that only changed
 * a preview, with the offer still built from the button that opened the screen,
 * would make the callee's `Video call` line a lie the moment anyone touched it.
 *
 * ## What the rewritten §3 removed from this module
 *
 * There used to be a second rule here — `prejoinDefaults`, which decided
 * whether the **pre-join screen** opened with the camera on. §3.1 retired that
 * screen for a voice call ("no preparation screen, no 'check your microphone',
 * no empty camera box") and §3.2 replaced it for a video call with the camera
 * check, which by definition opens with the camera on. A default for a screen
 * that only exists in one state is not a decision, so the function is gone
 * rather than kept as a constant nobody reads.
 *
 * ## Why this is a module
 *
 * Because it is a rule and not markup, and `vitest` cannot reach markup here —
 * the config runs a plain `node` environment with no jsdom and no Solid JSX
 * transform, so nothing in `routes/Call.tsx` is testable. The same reasoning
 * that moved the encryption chip's rule out of the JSX
 * (`calls/encryption-chip.ts`) applies to this one, and this one is the rule
 * whose failure is a false statement to the person being called.
 *
 * ## Audio is not a choice here, and that differs from the natives
 *
 * Every Lakko call captures the microphone; the pre-join mic toggle mutes the
 * captured track rather than declining to capture it, so `audio` is `true` on
 * every offer. That is why {@link mediaForPrejoin} takes the mic state but does
 * not fold it into `audio`: on the web `media.audio` decides whether
 * `getUserMedia` captures at all AND whether the offer carries an audio
 * section, so a call placed muted would have no audio transceiver and could
 * never be unmuted without a renegotiation. "Join muted" is not "join with no
 * microphone at all".
 *
 * iOS (`CallMediaChoice.offeredMedia`) and Android (`offeredCallMedia`) pass
 * the mic state straight through to `audio`, where it means something else in
 * their capture stacks. A deliberate divergence in mechanics — the words and
 * the states are identical, and it is the camera half that §3 is about.
 */

/** Which of the chat header's two call buttons was pressed. */
export type CallStartKind = "audio" | "video";

/** The media flags an offer carries, and `CallSession.dial` consumes. */
export interface CallMediaChoice {
  readonly audio: boolean;
  readonly video: boolean;
}

/** Where the camera check's toggles stand when the user presses Call. */
export interface CameraCheckToggles {
  readonly micOn: boolean;
  readonly camOn: boolean;
}

/**
 * The media a button press asks for, with no screen in between.
 *
 * §3.1's voice call: the phone button dials straight through, so the button IS
 * the whole choice.
 */
export function mediaForStart(kind: CallStartKind): CallMediaChoice {
  return { audio: true, video: kind === "video" };
}

/**
 * The media the offer carries after the user has had the camera check.
 *
 * Reads the **toggles**, not the button: once a screen has offered the user a
 * camera switch, the switch is the answer. A video call whose camera was turned
 * off in the check becomes a voice call, and the callee is told that instead.
 */
export function mediaForToggles(toggles: CameraCheckToggles): CallMediaChoice {
  return { audio: true, video: toggles.camOn };
}

/**
 * The word for a call of this shape, as the callee's screen and the call row
 * both use it.
 *
 * Derived from the media flags rather than carried alongside them, so the word
 * and the offer cannot disagree — which is the whole of §1's "always".
 */
export function callKindWord(media: CallMediaChoice): "Video call" | "Voice call" {
  return media.video ? "Video call" : "Voice call";
}
