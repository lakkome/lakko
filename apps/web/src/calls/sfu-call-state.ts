// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Asking the SFU whether a call is still there (`services/sfu` call
 * lifetime; `docs/ux/calls.md` section 4.3).
 *
 * A group call's record lives in the chat's MLS group and a member whose app
 * died publishes nothing, so after a crash the call it was alone in stays
 * "in progress" on every other phone for the freshness window, and whoever
 * taps Join lands in an empty room. The one party that knows a room is empty
 * is the SFU: the call ends there five seconds after the last socket goes,
 * and `GET /v1/calls/{call_id}?token=<call jwt>` says so. A member showing the
 * call as joinable asks every {@link SFU_CALL_STATE_PROBE_MS} and, on `ended`,
 * closes the chat's record itself by publishing the End signal — so the banner
 * goes on every device, not only its own.
 *
 * Pure pieces here, so the URL and the answer are tested without a network.
 */

/** How often a device offering Join asks the SFU. Chosen: a stale banner
 *  lives at most this long past the SFU's own five-second grace. */
export const SFU_CALL_STATE_PROBE_MS = 10_000;

/** What the SFU knows about a call id. */
export type SfuCallState = "unknown" | "live" | "closing" | "ended";

/**
 * The state route for `callIdHex`, on the HTTP origin behind the SFU's
 * WebSocket base (`wss://host/sfu` -> `https://host/sfu`). The token is the
 * same call-admission JWT the socket uses, bound to this call id.
 */
export function sfuCallStateUrl(sfuBaseUrl: string, callIdHex: string, jwt: string): string {
  const base = sfuBaseUrl.replace(/^wss:/i, "https:").replace(/^ws:/i, "http:").replace(/\/+$/, "");
  return `${base}/v1/calls/${callIdHex}?token=${encodeURIComponent(jwt)}`;
}

/** The route's answer, or `null` for anything that is not one of the four
 *  words (a proxy error page, an old server) — the caller then changes
 *  nothing, since a probe that cannot be read must never end a live call. */
export function parseSfuCallState(body: unknown): SfuCallState | null {
  if (typeof body !== "object" || body === null) return null;
  const state = (body as { state?: unknown }).state;
  switch (state) {
    case "unknown":
    case "live":
    case "closing":
    case "ended":
      return state;
    default:
      return null;
  }
}
