// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signaling keepalive for the SFU call session.
 *
 * The `lakko-sfu-v1` WebSocket has had an application-layer `Ping` / `Pong`
 * pair since the signaling client was written, but nothing in production sent
 * the ping and the session never listened for the pong. A dead SFU link was
 * therefore undetectable from the browser: the call timer kept counting on a
 * socket nobody answered, because a TCP connection that silently stops
 * delivering does not close itself for a long time.
 *
 * The rule below is what the session evaluates on a coarse tick. It is a pure
 * function of three timestamps, so it can be tested without a socket, and it
 * is shared word-for-word with iOS and Android, so the names and numbers must
 * stay exactly as they are here.
 */

/** Chosen, not measured: three missed pings before the link is declared dead. */
export const SFU_KEEPALIVE_PING_INTERVAL_MS = 15_000;
export const SFU_KEEPALIVE_DEADLINE_MS = 45_000;
/** How often the session re-evaluates; coarse on purpose. */
export const SFU_KEEPALIVE_TICK_MS = 5_000;

export type SfuKeepaliveAction = "ping" | "wait" | "dead";

/**
 * `lastInboundMs` is the last time the server proved it is there (the Welcome
 * frame or a Pong); `lastPingMs` the last time we asked. `dead` wins over `ping`.
 */
export function sfuKeepaliveAction(
  nowMs: number,
  lastInboundMs: number,
  lastPingMs: number,
): SfuKeepaliveAction {
  if (nowMs - lastInboundMs >= SFU_KEEPALIVE_DEADLINE_MS) return "dead";
  if (nowMs - lastPingMs >= SFU_KEEPALIVE_PING_INTERVAL_MS) return "ping";
  return "wait";
}

/**
 * When a call being recorded actually ended.
 *
 * `lastInboundMs` is the last time the server proved it was there. A device
 * that notices the end only after the keepalive deadline has passed since that
 * proof was asleep, or its link died and the tick ran late; either way the call
 * ended when the link did, not when the device woke up. Two devices left in a
 * call overnight (their End taps had not landed) each wrote an
 * `8:20:53` row the next morning at the moment their laptop woke: the row must
 * say how long the two sides were connected, never how long a screen sat
 * open. Shared word-for-word with iOS and Android.
 *
 * A `lastInboundMs` of 0 means the link never came up; the caller's clock is
 * then the only time there is.
 */
export function callEndedAtMs(nowMs: number, lastInboundMs: number): number {
  if (lastInboundMs <= 0) return nowMs;
  return nowMs - lastInboundMs > SFU_KEEPALIVE_DEADLINE_MS ? lastInboundMs : nowMs;
}
