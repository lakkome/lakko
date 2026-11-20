// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a finished call leaves behind (`docs/ux/calls.md` §6): the record of one
 * call, and the words the conversation shows for it.
 *
 * ## The defect this closes
 *
 * "A finished call leaves no trace in the conversation" — the spec's own
 * summary of the state it was written against. A call that rang and was missed
 * left the chat exactly as it was, so the only way to learn somebody had tried
 * to reach you was to have been looking at the screen while they did.
 *
 * ## The same four words as iOS and Android
 *
 * The design system allows the three clients to diverge in mechanics and never
 * in vocabulary or states, so the outcomes here are the ones
 * `apps/android/.../calls/CallTranscriptRow.kt` and
 * `apps/ios/Lakko/Calls/CallTranscript.swift` already ship: four rows,
 * `Outgoing call` / `Incoming call` / `Missed call` / `Declined`, a duration
 * appended after a comma when there was one, and nothing else. What the
 * platforms may differ on — where the row is stored, whether a tap is a click
 * or a double tap — they do.
 *
 * An earlier draft of this module split the unanswered cases further
 * (`No answer` for a call that rang out, `Cancelled` for one the caller gave
 * up on). That contradicted both the natives and §2.1, which is explicit:
 * "Ring for 45 seconds, then stop and **record a missed call for both sides**."
 * §3's `No answer` is the live call screen's wording for the caller while it is
 * happening, not the transcript's afterwards.
 *
 * ## Local, and only local
 *
 * A row here is this device's account of a call **it was in**. Nothing is
 * published, nothing is read from a peer, and no wire format changes: the two
 * sides of a call each write their own row from what their own session
 * observed, which is why the two rows differ (one says `Outgoing call`, the
 * other `Incoming call`) and why neither has to be believed by the other. A
 * device that was not in the call writes nothing — a second device of the same
 * account has no row for a call it did not take, and inventing one would be
 * claiming knowledge it does not have.
 *
 * ## `12:04` is a duration, not a clock time
 *
 * §4's status-line table fixes the token: "Connected | The running time", with
 * `12:04` as its example alongside `Ringing...` and `Connecting...`. §6 reuses
 * the same token in `Outgoing call, 12:04`, so it is the same quantity — how
 * long the call lasted, not when it started. The timeline already stamps every
 * row with its clock time (`chat/timeline-layout.ts` `shortTime`), so a second
 * clock time would be the same fact twice.
 *
 * It follows that the duration appears only where media actually flowed. A
 * call that never connected says only which way it went, or that it was missed
 * or declined — never `Outgoing call, 0:00`, which would report a
 * conversation that did not happen.
 */

import { t } from "../l10n";
import type { CallStartKind } from "./call-media-choice";
import { durationWords } from "./duration-words";

/**
 * Which of §6's four rows a finished call writes.
 *
 * Direction is folded in rather than carried beside the outcome, because three
 * of the four rows are already direction-specific and the fourth (`declined`)
 * reads the same from either side.
 */
export type CallOutcome =
  /** This device placed the call. */
  | "outgoing"
  /** This device answered the call. */
  | "incoming"
  /** An incoming call never answered here: it rang out, the caller gave up
   *  first, or this device was already busy. */
  | "missed"
  /** Refused on purpose — by this device, or by the other side. */
  | "declined"
  /** Answered here, and the connection never came up. Not missed: the person
   *  was there. */
  | "failed";

/** One call, as this device saw it. */
export interface CallLogEntry {
  /** The call id in hex. One row per call: re-recording the same id replaces
   *  rather than appends, so a duplicated terminal report cannot double the
   *  row. */
  readonly id: string;
  readonly outcome: CallOutcome;
  /** The call carried video. Picks the row's icon and the kind of call a tap
   *  places back; never part of its words. */
  readonly video: boolean;
  /** When the call started, as a Unix milliseconds stamp. Where the row sits
   *  in the timeline. */
  readonly startedAtMs: number;
  /** How long media flowed, in milliseconds, or `null` when it never did.
   *  Never a stand-in for "no answer". */
  readonly durationMs: number | null;
}

/**
 * A call with no conversation (`docs/ux/navigation.md` section 5.1.1): one
 * started with `New call`, or one this device joined from a call link.
 *
 * The same parts as {@link CallLogEntry}, so the row's words come from the
 * same {@link callRowLabel}, narrowed where such a call cannot go: it never
 * rings, so it is never missed or declined, and this device only writes one
 * after it was in the call. The `id` is the call id in hex, which is what
 * keeps it to one row per call on a device. It carries no name: a call has
 * none of its own, so every such row reads `Group call`.
 */
export interface StandaloneCallEntry extends CallLogEntry {
  /** `outgoing` when this device started the call with `New call` (or founded
   *  it from its own link), `incoming` when it joined from a link. */
  readonly outcome: "outgoing" | "incoming";
}

/** What the session reported when it reached its terminal state. */
export interface CallOutcomeInput {
  /** This device was being called. */
  readonly incoming: boolean;
  /** Media flowed: the call was actually answered. */
  readonly connected: boolean;
  /** This device pressed Accept. */
  readonly accepted?: boolean;
  /** The session's end reason (`calls/call-session.ts` `WebCallEndReason`), or
   *  `undefined` when it ended without giving one. */
  readonly reason: "hungUp" | "busy" | "declined" | "timedOut" | "error" | undefined;
}

/**
 * Decide the outcome from what the call host observed — the same three
 * questions, in the same order, as `callOutcome` on Android.
 *
 * Each step answers something the next one cannot ask:
 *
 * - A **deliberate refusal** by either side is `declined`, whichever way the
 *   call was going: the user chose this, and it is the one outcome that is not
 *   an accident. `declined` reads the same from both seats because it is the
 *   same event seen twice.
 * - An **incoming call that was never answered** is `missed`, whether it rang
 *   out, the caller gave up first, or this device was busy — from the callee's
 *   seat those are the same fact, and §2.1 records a missed call for both
 *   sides of a call nobody answered. One this device accepted and that never
 *   connected is `failed`: the person answered.
 * - Everything else is simply which way the call went. A call that connected
 *   is always its direction's row however it then ended: hanging up is how
 *   calls normally finish, and the row is not the place to relitigate it.
 */
export function classifyCallOutcome(input: CallOutcomeInput): CallOutcome {
  if (input.reason === "declined") return "declined";
  if (input.incoming && !input.connected) return input.accepted === true ? "failed" : "missed";
  return input.incoming ? "incoming" : "outgoing";
}

/**
 * `12:04`, or `1:02:33` once a call runs past an hour. `null` for a duration
 * that never happened or a clock that moved backwards mid-call — which reads
 * as no duration rather than as a nonsense number.
 *
 * Seconds are shown rather than rounded away because a call can genuinely last
 * eleven seconds, and `0:11` says so where "less than a minute" would not.
 * Minutes are not zero-padded past the hour mark, the way a phone's call log
 * writes them; seconds always are, or `1:2:3` would read as three numbers.
 */
export function formatCallDuration(durationMs: number | null): string | null {
  if (durationMs === null || durationMs < 0) return null;
  const total = Math.floor(durationMs / 1000);
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const ss = String(seconds).padStart(2, "0");
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, "0")}:${ss}`;
  return `${minutes}:${ss}`;
}

/** The one line the row shows (§6). */
export function callRowLabel(entry: CallLogEntry): string {
  const duration = formatCallDuration(entry.durationMs);
  switch (entry.outcome) {
    case "outgoing":
      return duration === null
        ? t("call.log.outgoing")
        : t("call.log.outgoing_duration", { duration });
    case "incoming":
      return duration === null
        ? t("call.log.incoming")
        : t("call.log.incoming_duration", { duration });
    case "missed":
      // Unanswered: there is no duration to report.
      return t("call.log.missed");
    case "declined":
      return t("call.log.declined");
    case "failed":
      return t("call.log.failed");
  }
}

/**
 * Whether the row is drawn in the danger tint — the outcomes where nobody
 * spoke. Never the only signal: the label says it too (§8).
 */
export function callRowIsUnanswered(entry: CallLogEntry): boolean {
  return entry.outcome === "missed" || entry.outcome === "declined" || entry.outcome === "failed";
}

/**
 * The kind of call a tap on the row places (§6: "Tapping the row calls back").
 *
 * Calls back in the kind the original call was, so returning a video call
 * offers video and returning a voice call does not silently turn the camera
 * on — the same honesty rule as the header buttons
 * (`calls/call-media-choice.ts`), and what `onCallBack(entry.video)` does on
 * Android.
 */
export function callBackKind(entry: CallLogEntry): CallStartKind {
  return entry.video ? "video" : "audio";
}

/**
 * A duration read as a duration (§8: "the running time is announced as a
 * duration, not read digit by digit"). `null` when there was none.
 */
export function spokenCallDuration(durationMs: number | null): string | null {
  if (durationMs === null || durationMs < 0) return null;
  return durationWords(Math.floor(durationMs / 1000), false);
}

/**
 * What a screen reader announces for the row (§8).
 *
 * The same sentence Android reads, minus its "Double tap" — that instruction
 * is a touch gesture, and on the web the row is a `<button>` whose role
 * already tells the reader how to press it. What survives is the part that is
 * vocabulary rather than mechanics: the kind of call, which way it went, and
 * that pressing calls back.
 */
export function callRowAccessibleLabel(entry: CallLogEntry): string {
  const video = entry.video;
  let head: string;
  switch (entry.outcome) {
    case "outgoing":
      head = video ? t("call.log.a11y.video_outgoing") : t("call.log.a11y.voice_outgoing");
      break;
    case "incoming":
      head = video ? t("call.log.a11y.video_incoming") : t("call.log.a11y.voice_incoming");
      break;
    case "missed":
      head = video ? t("call.log.a11y.video_missed") : t("call.log.a11y.voice_missed");
      break;
    case "declined":
      head = video ? t("call.log.a11y.video_declined") : t("call.log.a11y.voice_declined");
      break;
    case "failed":
      head = t("call.log.failed");
      break;
  }
  const spoken = spokenCallDuration(entry.durationMs);
  const described = spoken === null ? head : t("call.log.a11y.lasted", { head, duration: spoken });
  return t("call.log.a11y.call_back", { head: described });
}
