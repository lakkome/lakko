// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ICE servers of a call: Lakko's own, and nobody else's.
 *
 * Until 2026-10-24 every call, one-to-one and group, on all three clients,
 * started from Google's public STUN servers (`stun.l.google.com`), which told
 * Google each person's network address and when they called. The rule now:
 *
 *   - the TURN entry `services/auth` mints with the call admission, as given;
 *   - a STUN entry on the host and port of each of its `turn:` URIs, since
 *     Lakko's coturn answers STUN there (measured on the test stack
 *     2026-10-24: a binding request to edge2.lakko.me:3478 got a success
 *     response). `turns:` URIs give none: STUN is UDP, and TLS is another
 *     port;
 *   - nothing else. Without a TURN entry (no TURN configured, or minting
 *     failed) the list is empty: host candidates only. A group call still
 *     connects then, since the SFU learns the person's outside address from
 *     their own packets; a one-to-one call across two NATs may not, which is
 *     what the TURN entry is for.
 *
 * iOS and Android follow the same rule (`IceServers.swift`, `IceServers.kt`).
 */

/** The STUN URLs on the hosts of `turn:` URIs: `stun:host:port`, the port
 *  3478 when the URI names none, each host and port once. Pure. */
export function stunUrlsFromTurn(urls: string | readonly string[]): string[] {
  const list = typeof urls === "string" ? [urls] : urls;
  const out: string[] = [];
  for (const url of list) {
    if (!url.toLowerCase().startsWith("turn:")) continue;
    const rest = url.slice("turn:".length).split("?")[0] ?? "";
    const hostPort = parseHostPort(rest);
    if (hostPort === null) continue;
    const stun = `stun:${hostPort.host}:${hostPort.port}`;
    if (!out.includes(stun)) out.push(stun);
  }
  return out;
}

function parseHostPort(s: string): { host: string; port: number } | null {
  if (s.length === 0) return null;
  let host: string;
  let portText: string | undefined;
  if (s.startsWith("[")) {
    const end = s.indexOf("]");
    if (end < 0) return null;
    host = s.slice(0, end + 1);
    const after = s.slice(end + 1);
    if (after.length > 0) {
      if (!after.startsWith(":")) return null;
      portText = after.slice(1);
    }
  } else {
    const colon = s.lastIndexOf(":");
    if (colon >= 0) {
      host = s.slice(0, colon);
      portText = s.slice(colon + 1);
    } else {
      host = s;
    }
  }
  if (host.length === 0 || host === "[]") return null;
  if (portText === undefined) return { host, port: 3478 };
  if (!/^\d{1,5}$/.test(portText)) return null;
  const port = Number(portText);
  return port > 0 && port <= 65535 ? { host, port } : null;
}

/** A call's ICE servers from the minted TURN entry, or none. Pure. */
export function iceServersFor(turn: RTCIceServer | null): RTCIceServer[] {
  if (turn === null) return [];
  const stun = stunUrlsFromTurn(turn.urls);
  return stun.length === 0 ? [turn] : [{ urls: stun }, turn];
}

/** The servers' URLs, for the call log: no username, no credential. */
export function iceServersLine(servers: readonly RTCIceServer[]): string {
  const urls = servers.flatMap((s) => (typeof s.urls === "string" ? [s.urls] : [...s.urls]));
  return urls.length === 0 ? "none (host candidates only)" : urls.join(", ");
}
