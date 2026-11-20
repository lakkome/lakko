// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the `/g/` page shows around a request to join
 * (`docs/ux/group-invite-page.md`, "After `Request to join`", and
 * `docs/ux/group-and-channel-privacy.md` section 6.2, amended 2026-11-12).
 * In a plain module because `vitest.config.ts` cannot test a `.tsx` file.
 *
 * - No request from this device for this group: `Request to join`.
 * - Sent under a day ago (just now, or the page opened again within the
 *   day): "Request sent. An admin will see it when they next open Lakko." and
 *   no control.
 * - Sent a day ago or more: "No answer yet." and `Ask again`, which sends the
 *   same request as the `Requests sent` list's `Ask again` and restarts the
 *   day.
 * - The group landed: the page opens the chat at once, no `Open chat` step.
 */
import { askAgainState } from "../chat/ask-again";

/** The page's own progress through one visit. */
export type JoinPhase = "idle" | "requesting" | "waiting" | "joined" | "error";

/** What the signed-in part of the card shows. */
export type JoinPageView =
  /** `Request to join`. */
  | "request"
  /** The first request is on its way. */
  | "sending"
  /** The waiting line, no control. */
  | "waiting"
  /** "No answer yet." and `Ask again`. */
  | "no-answer"
  /** The group landed; the chat is opening. */
  | "joined"
  /** The request failed; the failure line and (when offered) `Try again`. */
  | "error";

export function joinPageView(input: {
  readonly phase: JoinPhase;
  /** When this device last sent a request to this group (the `Requests sent`
   *  record), or `null` when it holds none. */
  readonly sentAtMs: number | null;
  readonly nowMs: number;
}): JoinPageView {
  switch (input.phase) {
    case "requesting":
      return "sending";
    case "joined":
      return "joined";
    case "error":
      return "error";
    default:
      break;
  }
  if (input.sentAtMs !== null) return askAgainState(input.sentAtMs, input.nowMs);
  // Sent on this visit but not recorded (no account bound yet): it was sent
  // just now, so it waits.
  return input.phase === "waiting" ? "waiting" : "request";
}

/** The timer functions the wait uses, so a test can drive it. */
export interface WaitTimers {
  setInterval(fn: () => void, ms: number): number;
  clearInterval(id: number): void;
}

/**
 * Wait for the group to appear on this device (the mailbox receive loop
 * joins the Welcome and registers it), then call `onLanded` once. ADR-0053
 * contract point 6: a claimed journal answers only its members, so there is
 * nothing to read on the group side. Returns the function that stops the
 * wait.
 */
export function waitForGroup(input: {
  readonly landed: () => boolean;
  readonly onLanded: () => void;
  readonly timers: WaitTimers;
  readonly intervalMs?: number;
}): () => void {
  let stopped = false;
  const stop = (): void => {
    if (stopped) return;
    stopped = true;
    input.timers.clearInterval(id);
  };
  const id = input.timers.setInterval(() => {
    if (stopped || !input.landed()) return;
    stop();
    input.onLanded();
  }, input.intervalMs ?? 1000);
  return stop;
}
