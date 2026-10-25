// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which network path a call's media takes, as one log line.
 *
 * A delay spike that hits both directions at once (a burst of concealed
 * audio with nothing lost, measured on a one-to-one call on 2026-10-25) is
 * either the local network or a relay in between, and nothing in the logs
 * said which. This line does: the selected ICE candidate pair's local and
 * remote candidate types and protocols, the transport to the TURN server
 * for a relayed local candidate, and the pair's round-trip time.
 *
 *   call path: local=relay/udp/tls via=edge.lakko.me remote=host/udp rtt=41ms
 *
 * No address or port of either side is printed. `via=` names the TURN
 * server, which is Lakko's own, by its host name, and only when it is a
 * name: a URI naming an address prints nothing there.
 *
 * The line is written when the selected pair is first known and when the
 * path changes (another pair, or another type or protocol), not on every
 * statistics reading; a changed round-trip time alone is not a new path.
 *
 * The same rule and line are in iOS `CallPath.swift` and Android
 * `CallPath.kt`. Pure, so vitest can pin it.
 */

/** The selected pair of a statistics report, reduced to what the line prints. */
export interface CallPath {
  /** `<type>/<protocol>[/<relayProtocol>]` of the local candidate. */
  readonly local: string;
  /** `<type>/<protocol>` of the remote candidate. */
  readonly remote: string;
  /** The TURN server's host name for a relayed local candidate, else null. */
  readonly via: string | null;
  /** The pair's current round-trip time in milliseconds, when reported. */
  readonly rttMs: number | null;
  /** What identifies the path for change detection (no round-trip time). */
  readonly key: string;
}

type Stat = Readonly<Record<string, unknown>>;

function str(stat: Stat | undefined, key: string): string | null {
  const v = stat?.[key];
  return typeof v === "string" && v.length > 0 ? v : null;
}

function selectedPair(byId: ReadonlyMap<string, Stat>, entries: readonly Stat[]): Stat | undefined {
  for (const s of entries) {
    if (s["type"] !== "transport") continue;
    const id = str(s, "selectedCandidatePairId");
    const pair = id === null ? undefined : byId.get(id);
    if (pair !== undefined) return pair;
  }
  const pairs = entries.filter((s) => s["type"] === "candidate-pair");
  const succeeded = (s: Stat): boolean => s["state"] === "succeeded";
  return (
    pairs.find((s) => s["selected"] === true) ??
    pairs.find((s) => succeeded(s) && s["nominated"] === true) ??
    pairs.find(succeeded)
  );
}

/** The host of a TURN URI (`turn:host:port?...`), or null when it names an
 *  address rather than a host name, or does not parse. */
export function turnHostName(url: string | null): string | null {
  if (url === null) return null;
  const m = /^turns?:([^?]*)/i.exec(url);
  if (m === null) return null;
  const hostPort = m[1] ?? "";
  if (hostPort.startsWith("[")) return null;
  const host = hostPort.split(":")[0] ?? "";
  if (host.length === 0 || /^[0-9.]+$/.test(host)) return null;
  return /^[A-Za-z0-9.-]+$/.test(host) ? host.toLowerCase() : null;
}

/** The selected pair of a `getStats()` report, or null before there is one. */
export function readCallPath(entries: readonly Stat[]): CallPath | null {
  const byId = new Map<string, Stat>();
  for (const s of entries) {
    const id = str(s, "id");
    if (id !== null) byId.set(id, s);
  }
  const pair = selectedPair(byId, entries);
  if (pair === undefined) return null;
  const localId = str(pair, "localCandidateId");
  const remoteId = str(pair, "remoteCandidateId");
  const local = localId === null ? undefined : byId.get(localId);
  const remote = remoteId === null ? undefined : byId.get(remoteId);
  if (local === undefined || remote === undefined) return null;
  const localType = str(local, "candidateType") ?? "?";
  const relayProtocol = localType === "relay" ? str(local, "relayProtocol") : null;
  const localText =
    `${localType}/${str(local, "protocol") ?? "?"}` + (relayProtocol === null ? "" : `/${relayProtocol}`);
  const remoteText = `${str(remote, "candidateType") ?? "?"}/${str(remote, "protocol") ?? "?"}`;
  const via = localType === "relay" ? turnHostName(str(local, "url")) : null;
  const rtt = pair["currentRoundTripTime"];
  const rttMs = typeof rtt === "number" && Number.isFinite(rtt) && rtt >= 0 ? Math.round(rtt * 1000) : null;
  return {
    local: localText,
    remote: remoteText,
    via,
    rttMs,
    key: `${str(pair, "id") ?? "?"}|${localText}|${via ?? ""}|${remoteText}`,
  };
}

/** The log line for a path. */
export function describeCallPath(path: CallPath): string {
  return (
    `call path: local=${path.local}` +
    (path.via === null ? "" : ` via=${path.via}`) +
    ` remote=${path.remote} rtt=${path.rttMs === null ? "-" : `${path.rttMs}ms`}`
  );
}

/** One call's path log: a line when the path is first known or changes. */
export class CallPathLog {
  private lastKey: string | null = null;

  /** The line to log for this reading, or null when the path is unchanged
   *  or not known yet. */
  note(entries: readonly Stat[]): string | null {
    const path = readCallPath(entries);
    if (path === null || path.key === this.lastKey) return null;
    this.lastKey = path.key;
    return describeCallPath(path);
  }
}
