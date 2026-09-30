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
 * The pre-call screen then survived for a **video** call only, as §3.2's
 * camera check — a live self-view filling the screen, three round buttons over
 * it — and that went too on 2026-09-21. §3.2 now reads the other way: one
 * press of a call button is one call, for both kinds. The check's reason ("a
 * person about to be seen deserves to know what they look like") is not
 * abandoned; it is already met by the self-view on the calling screen, which
 * shows the same thing a moment later without a second press.
 *
 * ## Why the camera is still probed before dialling
 *
 * §3.2's last sentence survives: "If the device has no camera, the video button
 * starts a voice call and says so in one line." The 1x1 session asks for both
 * tracks in one `getUserMedia` and has no §4.4-style fallback of its own, so a
 * camera-less device would fail the whole request and place no call at all. The
 * caller therefore probes first and {@link planCallStart} is told the answer —
 * the same probe the check used to need, kept for a different reason.
 *
 * ## Why this is a module
 *
 * `vitest` has no jsdom and no Solid transform here, so a rule written inside
 * `routes/Chat.tsx` — 300 KB of one component — is a rule no test can reach.
 * The rule that decides whether a person is asked a question before their phone
 * call is worth reaching.
 */

import type { CallMediaChoice, CallStartKind } from "./call-media-choice";
import { t } from "../l10n";
import type { CallText } from "./call-slot";

/** The one line shown when the video button could not open a camera (§3.2). */
export const CAMERA_UNAVAILABLE_NOTICE: CallText = () => t("call.start.no_camera");

/**
 * What a press of a call button leads to.
 *
 * One variant, and that is the decision: every call button dials. The union is
 * kept so the call sites keep asking one question in one place, and so a
 * future pre-call surface has somewhere to be added. `camera-check` was the
 * other variant until 2026-09-21.
 */
export type CallStartPlan =
  /** Place the call now. `notice` is the one line §3.2 owes the user when the
   *  call they asked for is not the call they are getting. */
  { readonly kind: "dial"; readonly media: CallMediaChoice; readonly notice: CallText | null };

/**
 * What the chat header's two buttons do.
 *
 * Both dial, immediately (§3.1, and §3.2 since 2026-09-21). The video button
 * carries the camera when there is one, and otherwise places the voice call it
 * can actually place, saying so in the one line §3.2 owes.
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
  return { kind: "dial", media: { audio: true, video: true }, notice: null };
}
