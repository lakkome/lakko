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
 * one quiet line) and a **video fill** (a picture edge to edge, the self-view in
 * a corner). Which one to draw is a question about *what there is to look at*,
 * not about what the call was placed as — §4.1: "Pressing it starts the camera,
 * tells the other side, and the screen becomes the video layout for both ...
 * Turning the camera off again returns the call to the portrait." A call is
 * neither a voice call nor a video call for its whole life; it is whichever one
 * it is at this moment.
 *
 * So the surface follows **any** camera that is on, and {@link callFillSource}
 * then says whose picture fills it: the other person's if they are sending one,
 * otherwise this device's own. The pair is what keeps §4's "never an empty box"
 * true in both directions — a person who turns their camera on while the other
 * has not is shown the thing they just started, and the person receiving it is
 * shown the incoming picture with no self-view, because their own camera is off.
 *
 * Nobody answered yet is the exception: while a video call is still ringing the
 * caller keeps the portrait of the person they are calling, with their own
 * preview in the corner. Filling that screen with the caller's own face would
 * take away the only thing on it that says who is being called.
 *
 * A group call is the third surface and keeps its roster grid — §9 puts a group
 * redesign out of scope, and the roster is what works today.
 */

import { t } from "../l10n";
import { durationWords } from "./duration-words";

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
  /** What KIND of call this is, not how many people are in it right now. A
   *  group call is the group surface from its first second (§4.4): the person
   *  who started it is its first participant and sees their own tile, never a
   *  portrait of a person who is not there. Defaults to `false` for the 1x1
   *  paths, which never set it. */
  readonly groupCall?: boolean;
  /** How many participants are not this device. */
  readonly remoteCount: number;
  /** Whether the sole remote is sending a picture. Ignored when
   *  `remoteCount !== 1`. */
  readonly remoteCameraOn: boolean;
  /** Whether this device is sending one. */
  readonly localCameraOn: boolean;
}

/**
 * The layout for this call, right now.
 *
 * A group call is the group surface whoever is in it yet (§4.4); on the 1x1
 * paths two or more remotes also means the roster grid. One remote and
 * a camera on at either end fills the screen (§4.1: the layout changes "for
 * both", and it changes back when the camera stops). Everything else — nobody
 * there yet, or two cameras off — is the portrait, because a portrait of the
 * person you are talking to is never an empty box.
 */
export function callSurfaceKind(input: CallSurfaceInput): CallSurfaceKind {
  if (input.groupCall === true) return "group";
  if (input.remoteCount >= 2) return "group";
  if (input.remoteCount === 1 && (input.remoteCameraOn || input.localCameraOn)) return "video";
  return "voice";
}

/** What {@link featuredParticipant} needs to know about one tile. The store's
 *  `Participant` satisfies it structurally; the rule reads nothing else. */
export interface FeaturedCandidate {
  readonly id: string;
  readonly isLocal: boolean;
  readonly camOn: boolean;
  readonly speaking: boolean;
}

/**
 * The tile a group call draws large, or `null` for an equal grid (§4.4).
 *
 * Pinned, else speaking with video, else the first in roster order with video,
 * else nobody. The user's own tile is never featured while anyone else is
 * present; alone, their preview is the only thing there is to show. `speaking`
 * is a hint the transport may never set, and the rule reads the same when it
 * is always `false`: the first camera in roster order.
 *
 * What this replaced picked the first remote whatever they were sending, so
 * the first person to join held the big slot for the whole call with an avatar
 * in it while other people's cameras ran in the small tiles.
 */
export function featuredParticipant<T extends FeaturedCandidate>(
  participants: readonly T[],
  pinnedId: string | null,
): T | null {
  if (pinnedId !== null) {
    const pinned = participants.find((p) => p.id === pinnedId);
    if (pinned !== undefined) return pinned;
  }
  const remotes = participants.filter((p) => !p.isLocal);
  if (remotes.length === 0) return participants.find((p) => p.isLocal) ?? null;
  return (
    remotes.find((p) => p.speaking && p.camOn) ?? remotes.find((p) => p.camOn) ?? null
  );
}

/**
 * The grid under (or instead of) the featured tile: everyone but the featured
 * one, in roster order; everyone, when there is none.
 */
export function galleryParticipants<T extends FeaturedCandidate>(
  participants: readonly T[],
  featured: T | null,
): T[] {
  if (featured === null) return [...participants];
  return participants.filter((p) => p.id !== featured.id);
}

/** What {@link callPeerLabel} needs to know. */
export interface CallPeerLabelInput {
  readonly surface: CallSurfaceKind;
  /** The other person, as the call host resolved them, or `null` on a group
   *  or link call where there is no single other person. */
  readonly peerName: string | null | undefined;
  /** Everyone on the call including this device — what the group label
   *  counts. */
  readonly participantCount: number;
  /** The sole remote's display name, for the 1x1 case where the host has not
   *  resolved one. */
  readonly soleRemoteName: string | null | undefined;
}

/**
 * Who is on the call, as the name at the top of the call screen (§4) and, since
 * §4.5, on the collapsed bar.
 *
 * Lifted out of `routes/Call.tsx` when the bar was built, and that is the whole
 * reason it is a function: §4.5 says the bar carries "the same label the call
 * screen uses", and two places computing the same name from the same facts is
 * exactly how they come to disagree. `vitest` here has no Solid transform, so
 * the copy that lived in the view was a rule nothing could assert.
 *
 * A group call has no single name, so the label names the call and counts it
 * instead of inventing a person. `Participant` for a 1x1 call the host could
 * not name is practically unreachable — it resolves a name for every one — but
 * it must not invent a person if it is, and `encryption-chip.ts` treats that
 * exact word as "no name" so the sheet's sentence stays consistent with it.
 */
export function callPeerLabel(input: CallPeerLabelInput): string {
  if (input.surface === "group") {
    return t("call.status.group_count", { count: input.participantCount });
  }
  const given = input.peerName?.trim() ?? "";
  if (given !== "") return given;
  const sole = input.soleRemoteName?.trim() ?? "";
  return sole !== "" ? sole : t("call.participant.unnamed");
}

/** Whose picture fills the video surface. */
export type CallFillSource = "remote" | "local" | "none";

/**
 * What the video surface is showing.
 *
 * The other person's picture whenever there is one — that is the call. Only
 * when there is not, and this device is the one sending, does the screen fall
 * back to its own camera: §4.1 says a person who turns their camera on gets the
 * video layout even though the other side has not, and the only picture in
 * existence at that moment is theirs.
 *
 * `none` on every non-video surface, so a caller cannot read this as "there is
 * something to draw" when there is not.
 */
export function callFillSource(
  surface: CallSurfaceKind,
  remoteCameraOn: boolean,
  localCameraOn: boolean,
): CallFillSource {
  if (surface !== "video") return "none";
  if (remoteCameraOn) return "remote";
  return localCameraOn ? "local" : "none";
}

/** What {@link callStatusDetail} needs to know. */
export interface CallStatusInput {
  /** A group call, or a call joined from a link. Nobody is being called in
   *  one, so its first seconds read `Connecting...` and never `Calling...`. */
  readonly groupCall?: boolean;
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
      return input.groupCall === true ? t("common.connecting") : t("call.status.calling");
    case "connecting":
      return t("common.connecting");
    case "reconnecting":
      return t("call.status.reconnecting");
    case "connected":
      return formatCallDuration(elapsedMs(input.connectedAtMs, input.nowMs));
    case "ended":
      return t("call.status.ended");
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
  return durationWords(Math.floor(Math.max(0, ms) / 1000), true);
}

/**
 * What the ended screen says (§4: "`Call ended` and the duration").
 *
 * A call that never connected has no duration, and inventing `0:00` for one
 * that was declined or never answered would state something false about it —
 * so the sentence simply stops.
 */
export function endedSummary(connectedAtMs: number | null, endedAtMs: number): string {
  if (connectedAtMs === null) return t("call.status.ended");
  return t("common.fact_pair", {
    first: t("call.status.ended"),
    second: formatCallDuration(elapsedMs(connectedAtMs, endedAtMs)),
  });
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
 * It follows the local camera, with two exclusions. There is none on the group
 * surface, where the local participant already has a roster tile. And there is
 * none when this device's own camera is already the thing filling the screen —
 * a corner copy of the picture behind it is not a second view, it is the same
 * view twice.
 *
 * §4.1's other half falls out of the same rule rather than needing its own: the
 * person receiving a picture they are not returning has their camera off, so
 * "their screen shows the incoming picture with their self-view absent" is what
 * `localCameraOn === false` already means.
 */
export function selfViewVisible(
  surface: CallSurfaceKind,
  localCameraOn: boolean,
  fill: CallFillSource,
): boolean {
  if (!localCameraOn) return false;
  if (surface === "group") return false;
  return fill !== "local";
}
