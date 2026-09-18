// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When a receive loop is allowed to stop receiving.
 *
 * ## The defect this rule replaces
 *
 * Both web receive loops parked on one condition — `document.visibilityState
 * === "hidden"` — and each carried the same justification in a comment: "push
 * covers a backgrounded tab, as on the native clients". The open chat's long
 * poll (`routes/Chat.tsx`) made one exception, a call already in progress; the
 * background receive (`BackgroundReceiveContext.tsx`) made two, a call in
 * progress and a 60 s window opened by a service-worker push wake.
 *
 * Neither exception covers the case that matters: a call ARRIVING at a tab
 * push cannot reach. Observed on 2026-09-18, iOS to web on test.lakko.me: the
 * callee's tab was on another chat, no full-screen incoming call appeared, no
 * `End{busy}` was published, and the caller rang for its full 45 s and
 * recorded a missed call. Nothing in the call host was wrong — the offer row
 * was never fetched from the DS journal at all, because every loop that would
 * have fetched it was parked, and the only signal that un-parks the background
 * sweep is a push wake the tab could not receive (notifications were denied).
 * The in-app test pane reports `visibilityState === "hidden"` even when
 * fronted, so the user was looking at a tab that had stopped receiving.
 *
 * `docs/ux/calls.md` section 2.1 states the fallback the old rule made
 * unreachable: "A browser that refuses notification permission shows the
 * in-app banner only". A tab with no push path has the in-app ring and
 * nothing else, so it has to keep receiving to have it.
 *
 * ## The rule
 *
 * **Park only when a push wake is known to reach this tab.** Stated as one
 * sentence because it is one sentence: the loops may sleep exactly when
 * something else can wake them, and never otherwise. `unknown` — the probe in
 * `push/wake-reach.ts` has not resolved — keeps receiving, because the cost of
 * staying awake is battery and the cost of parking wrongly is a call that can
 * never arrive.
 *
 * A live call still overrides everything, as before: 1x1 signaling rides the
 * group journal and a group call's mid-call rekey (ADR-0029) is driven by the
 * epoch advances a poll applies, so a parked loop during a call is a stalled
 * connection rather than a late message.
 *
 * Kept pure and in a `.ts` module for the reason every rule on this client is:
 * vitest runs in plain node with no Solid transform, so nothing inside a
 * `.tsx` provider can be tested — which is exactly where this condition lived
 * while it was wrong.
 */
import type { PushWakeReach } from "../push/wake-reach";

/** What decides whether a receive loop may park. */
export interface ReceiveParkInput {
  /** `document.visibilityState === "hidden"`. */
  readonly documentHidden: boolean;
  /** A call this loop's journal carries is ringing, starting or live. */
  readonly callInProgress: boolean;
  /** Whether a push wake can reach this tab (`push/wake-reach.ts`). */
  readonly pushWake: PushWakeReach;
  /** A push wake landed recently and its catch-up window is still open.
   *  Background receive only; the chat view has no such window. */
  readonly wakeWindowOpen?: boolean;
}

/**
 * May this loop stop receiving right now?
 *
 * `true` only for a hidden tab with no call in progress, no open wake window,
 * and a push path known to reach it.
 */
export function mayParkReceive(input: ReceiveParkInput): boolean {
  if (!input.documentHidden) return false;
  if (input.callInProgress) return false;
  if (input.wakeWindowOpen === true) return false;
  return input.pushWake === "reachable";
}

/** The background receive's phrasing of the same rule: is it sweeping? */
export function backgroundReceiveIsActive(
  input: ReceiveParkInput & { readonly signedIn: boolean },
): boolean {
  return input.signedIn && !mayParkReceive(input);
}
