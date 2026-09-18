// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a call actually asks for, from the button that started it through the
 * pre-join toggles (`docs/ux/calls.md` §3).
 *
 * ## The rule this exists to hold
 *
 * §1: "Say what kind of call it is. The word on the callee's screen matches
 * what the caller chose, always." §3 makes the mechanism explicit: "A voice
 * call never turns the camera on. The pre-join screen opens with the camera off
 * for a voice call and on for a video call, and the toggles there change the
 * call that is actually placed."
 *
 * Two separate claims, and both have to hold together:
 *
 * 1. The **default** on the pre-join screen follows the button the user
 *    pressed. The call surface used to open its camera preview on regardless,
 *    so pressing the phone button and pressing the video button produced the
 *    same screen — the defect §3 names.
 * 2. Whatever the user leaves those toggles at is what the **offer** carries.
 *    A toggle that only changes a preview, with the offer still built from the
 *    original button, would make the callee's `Video call` line a lie the
 *    moment anyone touched it.
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

/** Where the pre-join screen's toggles start. */
export interface PrejoinToggles {
  readonly micOn: boolean;
  readonly camOn: boolean;
}

/**
 * The media a button press asks for, with no pre-join screen in between.
 *
 * This is the 1x1 and group path on web today: the header button dials
 * straight through, so the button IS the whole choice.
 */
export function mediaForStart(kind: CallStartKind): CallMediaChoice {
  return { audio: true, video: kind === "video" };
}

/**
 * Where the pre-join screen opens for a given button — §3's "camera off for a
 * voice call and on for a video call".
 *
 * The microphone starts on either way: a call nobody can hear is not a state a
 * user means to place a call in, and the toggle is right there.
 */
export function prejoinDefaults(kind: CallStartKind): PrejoinToggles {
  return { micOn: true, camOn: kind === "video" };
}

/**
 * The media the offer carries after the user has had the pre-join screen.
 *
 * Reads the **toggles**, not the button: once a screen has offered the user a
 * camera switch, the switch is the answer. A voice call whose camera was turned
 * on becomes a video call and the callee is told so; a video call whose camera
 * was turned off becomes a voice call and the callee is told that instead.
 */
export function mediaForPrejoin(toggles: PrejoinToggles): CallMediaChoice {
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
