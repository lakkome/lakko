// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * SFU endpoint configuration for the web client.
 *
 * The Rust SFU is a WebSocket origin (`ws://` / `wss://`) with no
 * trailing path — `SfuSignalingClient` appends `/v1/connect`. The URL
 * is read from the Vite build-time env var `VITE_LAKKO_SFU_URL`.
 *
 * Returns `null` when unset, so the group-call entry point can refuse
 * with a clear message rather than dialing a bogus endpoint. The SFU
 * is a separate deployment from the `/api/*` services nginx fronts
 * (it terminates WebSocket + UDP media directly), so it does NOT ride
 * the relative `/api` base the other clients use.
 */

/** The configured SFU WebSocket origin, or `null` if unset. Trailing
 *  slashes are trimmed so callers can append `/v1/connect` cleanly. */
export function sfuBaseUrl(): string | null {
  // `import.meta.env` is typed loosely; access the custom var defensively.
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_SFU_URL"];
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim().replace(/\/+$/, "");
  return trimmed.length > 0 ? trimmed : null;
}
