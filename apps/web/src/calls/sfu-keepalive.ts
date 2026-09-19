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
