// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the call screen shows, as rules rather than markup (`docs/ux/calls.md`
 * §4).
 *
 * ## Why this is a module
 *
 * The same reason `calls/encryption-chip.ts` is one: `vitest` runs in a plain
 * `node` environment with no jsdom and no Solid JSX transform, so nothing in
 * `routes/Call.tsx` is reachable from a test. A decision written as a condition
 * inside JSX is a decision nobody can assert, and the encryption chip spent nine
 * months being wrong in exactly that position. Everything here — which surface a
 * call renders as, what the one status line says, whether the controls may fade
 * — is a decision, so it lives where a test can reach it.
 *
 * ## The three surfaces, and why the split is not "audio vs video"
 *
 * §4 describes two layouts for a 1x1 call: a **portrait** (large avatar, name,
 * one quiet line) and a **video fill** (the other person edge to edge, the
 * self-view in a corner). Which one to draw is a question about *what there is
 * to look at*, not about what the call was placed as: a video call whose callee
 * has their camera off has nothing to fill the screen with, and drawing a black
 * rectangle there is the "empty camera box" the redesign exists to remove. So
 * {@link callSurfaceKind} asks whether a remote picture exists, and the portrait
 * is the answer whenever it does not.
 *
 * A group call is the third surface and keeps its roster grid — §9 puts a group
 * redesign out of scope, and the roster is what works today.
 *
 * **This is deliberately NOT the same question as which buttons the row
 * carries.** That one *is* about the kind of call (§4.1's table: a voice call
 * carries no camera button), and it is answered by the call's media kind in
 * `calls/call-controls.ts`. Conflating the two would make the camera button
 * blink out of the row the moment the other person covered their lens.
 */

/** Which layout the call screen draws. */
export type CallSurfaceKind = "voice" | "video" | "group";

/** The call lifecycle, as the screen reads it. Mirrors `CallPhase["kind"]` in
 *  `state/call-state.ts`; kept as a plain string union here so this module
 *  stays free of the Solid store. */
export type CallPhaseKind =
  | "idle"
  | "calling"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "ended";

/** What {@link callSurfaceKind} needs to know. */
export interface CallSurfaceInput {
  /** How many participants are not this device. */
  readonly remoteCount: number;
  /** Whether the sole remote is sending a picture. Ignored when
   *  `remoteCount !== 1`. */
  readonly remoteCameraOn: boolean;
}

/**
 * The layout for this call, right now.
 *
 * Two or more remotes is a group call and keeps the roster grid. One remote
 * with a picture fills the screen. Everything else — nobody there yet, or a
 * remote with their camera off — is the portrait, because a portrait of the
 * person you are calling is never an empty box.
 */
export function callSurfaceKind(input: CallSurfaceInput): CallSurfaceKind {
  if (input.remoteCount >= 2) return "group";
  if (input.remoteCount === 1 && input.remoteCameraOn) return "video";
  return "voice";
}

/** What {@link callStatusDetail} needs to know. */
export interface CallStatusInput {
  readonly phase: CallPhaseKind;
  /** When media started flowing, or `null` before it has. */
  readonly connectedAtMs: number | null;
  readonly nowMs: number;
}

/**
 * The one line under the name (§4: "name, then state or duration ... Never two
 * of them at once").
 *
 * `Calling...` and `Connecting...` are different words for different facts and
 * the screen used to print the second for both: the call store collapsed the
 * dialing state into `connecting`, so a call that was ringing on the other
 * phone claimed to be negotiating. §4's state table names them separately, so
 * they are separate here.
 *
 * A connected call shows the running time and nothing else — no "Connected"
 * label competing with it. Before the first tick is known (the timer starts
 * when media flows, and a caller can reach `connected` a frame earlier) the
 * duration reads `0:00` rather than a placeholder that would flash.
 */
export function callStatusDetail(input: CallStatusInput): string {
  switch (input.phase) {
    case "idle":
      return "";
    case "calling":
      return "Calling...";
    case "connecting":
      return "Connecting...";
    case "reconnecting":
      return "Reconnecting...";
    case "connected":
      return formatCallDuration(elapsedMs(input.connectedAtMs, input.nowMs));
    case "ended":
      return "Call ended";
  }
}

/** Milliseconds of connected time, floored at zero so a clock that steps
 *  backwards cannot print a negative duration. */
export function elapsedMs(connectedAtMs: number | null, nowMs: number): number {
  if (connectedAtMs === null) return 0;
  return Math.max(0, nowMs - connectedAtMs);
}

/**
 * The running time as §4 writes it: `12:04`.
 *
 * Minutes carry no leading zero under an hour (`0:05`, not `00:05`) because
 * that is how every phone renders a call; past an hour it becomes `1:02:03` and
 * the minutes pad, so the field widths stay honest.
 */
export function formatCallDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const ss = `${seconds}`.padStart(2, "0");
  if (hours === 0) return `${minutes}:${ss}`;
  return `${hours}:${`${minutes}`.padStart(2, "0")}:${ss}`;
}

/**
 * The running time for a screen reader (§8: "the running time is announced as
 * a duration, not read digit by digit").
 *
 * `12:04` is read back as "twelve oh four", which is a time of day. This says
 * what it is.
 */
export function spokenCallDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const parts: string[] = [];
  if (hours > 0) parts.push(`${hours} ${hours === 1 ? "hour" : "hours"}`);
  if (minutes > 0) parts.push(`${minutes} ${minutes === 1 ? "minute" : "minutes"}`);
  parts.push(`${seconds} ${seconds === 1 ? "second" : "seconds"}`);
  return parts.join(" ");
}

/**
 * What the ended screen says (§4: "`Call ended` and the duration").
 *
 * A call that never connected has no duration, and inventing `0:00` for one
 * that was declined or never answered would state something false about it —
 * so the sentence simply stops.
 */
export function endedSummary(connectedAtMs: number | null, endedAtMs: number): string {
  if (connectedAtMs === null) return "Call ended";
  return `Call ended · ${formatCallDuration(elapsedMs(connectedAtMs, endedAtMs))}`;
}

/** How long a connected video call waits before the controls fade (§4). */
export const CONTROLS_IDLE_HIDE_MS = 5_000;

/** How long the ended screen stays up before it closes itself (§4). */
export const ENDED_DISMISS_MS = 2_000;

/** What {@link controlsHidden} needs to know. */
export interface ControlsVisibilityInput {
  readonly surface: CallSurfaceKind;
  readonly phase: CallPhaseKind;
  /** Milliseconds since the last pointer / key interaction with the call. */
  readonly msSinceInteraction: number;
}

/**
 * Whether the control bar has faded out (§4: "the controls fade out after 5
 * seconds of a connected video call and come back on a tap; on a voice call
 * they never hide").
 *
 * Only the video fill hides them, and only once connected. §4.2 gives the
 * reason the voice portrait never does: there is nothing underneath to look at,
 * so hiding the controls buys the user nothing and costs them a tap. The group
 * grid is treated as a voice call here for the same reason — the roster is not
 * a single picture the bar is standing in front of.
 */
export function controlsHidden(input: ControlsVisibilityInput): boolean {
  if (input.surface !== "video") return false;
  if (input.phase !== "connected") return false;
  return input.msSinceInteraction >= CONTROLS_IDLE_HIDE_MS;
}

/**
 * Whether the self-view rectangle is drawn.
 *
 * It follows the local camera and nothing else: on the video fill it is the
 * corner rectangle §4 describes, and on the portrait it is what a person who
 * has turned their own camera on expects to see — WhatsApp draws exactly that
 * when the other side has no picture. There is no self-view on the group
 * surface, where the local participant already has a roster tile.
 */
export function selfViewVisible(surface: CallSurfaceKind, localCameraOn: boolean): boolean {
  return localCameraOn && surface !== "group";
}
