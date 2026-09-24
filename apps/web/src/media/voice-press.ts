// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a press on a voice message's play button does — and, decisively, WHEN.
 *
 * This is one `switch` lifted out of `MediaBubbles.tsx` for one reason: the
 * defect it encodes is not about which branch is taken but about what may
 * happen before the branch runs.
 *
 * Every engine gates AUDIBLE playback on a user activation, and WebKit drops
 * that activation when the handler's own task ends. A press that awaited
 * anything before calling `play()` was therefore refused with
 * `NotAllowedError` — which `playback-outcome.ts` deliberately classifies as
 * `"tap-to-play"` for a voice message, because sound is the whole message and
 * there is no muted fallback worth offering. The button went back to the
 * triangle and nothing was said. A voice message could not be played, and the
 * failure was silent: the one path through the bubble that reports nothing at
 * all.
 *
 * So the rule the caller must keep, and the reason this returns a decision
 * rather than a promise:
 *
 *   * `"play-now"` must be acted on **synchronously**, on the handler's own
 *     stack, with no `await` in front of `play()`.
 *   * `"prepare-then-play"` must still touch the element synchronously —
 *     bringing it into existence and `load()`-ing it inside the gesture is
 *     what tells the engine the person asked for THIS element's sound — and
 *     only then fetch.
 *
 * Nothing here knows about elements, promises or the DOM, so the table can be
 * pinned by a test under bare node.
 */

/** The four things a press can mean. */
export type VoicePress =
  /** The message has not reached the other side yet; the button is disabled. */
  | "ignore"
  /** It is playing: stop it. */
  | "pause"
  /** The bytes are here — call `play()` on this stack, now. */
  | "play-now"
  /** The bytes are not here yet: unlock the element, then fetch, then start. */
  | "prepare-then-play";

export interface VoicePressState {
  /** An outgoing take still uploading. */
  readonly uploading: boolean;
  /** The element is playing right now. */
  readonly playing: boolean;
  /**
   * The element exists AND has its source, so `play()` can be called this
   * instant. Not "the bytes are fully buffered": an element that has a source
   * may still be loading, and `play()` waits for data by itself — what
   * matters is that nothing has to be awaited before the call.
   */
  readonly ready: boolean;
}

export function voicePress(state: VoicePressState): VoicePress {
  if (state.uploading) return "ignore";
  if (state.playing) return "pause";
  return state.ready ? "play-now" : "prepare-then-play";
}
