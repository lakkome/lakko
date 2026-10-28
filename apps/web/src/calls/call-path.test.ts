// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CallPathLog, describeCallPath, readCallPath, turnHostName } from "./call-path";

type Stat = Record<string, unknown>;

function report(opts: {
  pairId?: string;
  local?: Stat;
  remote?: Stat;
  rtt?: number;
  viaTransport?: boolean;
  state?: string;
}): Stat[] {
  const pairId = opts.pairId ?? "CP1";
  const entries: Stat[] = [
    {
      id: "L1",
      type: "local-candidate",
      candidateType: "host",
      protocol: "udp",
      address: "192.168.1.20",
      port: 50000,
      ...opts.local,
    },
    {
      id: "R1",
      type: "remote-candidate",
      candidateType: "host",
      protocol: "udp",
      address: "192.168.1.21",
      port: 50001,
      ...opts.remote,
    },
    {
      id: pairId,
      type: "candidate-pair",
      state: opts.state ?? "succeeded",
      nominated: true,
      localCandidateId: "L1",
      remoteCandidateId: "R1",
      currentRoundTripTime: opts.rtt ?? 0.012,
    },
  ];
  if (opts.viaTransport === true) entries.push({ id: "T1", type: "transport", selectedCandidatePairId: pairId });
  return entries;
}

describe("readCallPath", () => {
  it("prints a direct path with no address or port", () => {
    const path = readCallPath(report({}));
    expect(path).not.toBeNull();
    const line = describeCallPath(path!);
    expect(line).toBe("call path: local=host/udp remote=host/udp rtt=12ms");
    expect(line).not.toContain("192.168");
    expect(line).not.toContain("5000");
  });

  it("prints the TURN transport and server name of a relayed local candidate", () => {
    const path = readCallPath(
      report({
        local: {
          candidateType: "relay",
          protocol: "udp",
          relayProtocol: "tls",
          url: "turns:edge.lakko.me:443?transport=tcp",
          address: "46.224.156.97",
        },
        remote: { candidateType: "srflx" },
        rtt: 0.0414,
      }),
    );
    expect(describeCallPath(path!)).toBe(
      "call path: local=relay/udp/tls via=edge.lakko.me remote=srflx/udp rtt=41ms",
    );
  });

  it("follows the transport's selected pair when there is one", () => {
    const entries = report({ viaTransport: true, state: "in-progress" });
    expect(readCallPath(entries)?.local).toBe("host/udp");
  });

  it("is null before a pair has succeeded", () => {
    expect(readCallPath(report({ state: "in-progress" }))).toBeNull();
    expect(readCallPath([])).toBeNull();
  });

  it("prints a dash when no round-trip time is reported", () => {
    const entries = report({});
    delete (entries[2] as Stat)["currentRoundTripTime"];
    expect(describeCallPath(readCallPath(entries)!)).toBe("call path: local=host/udp remote=host/udp rtt=-");
  });
});

describe("turnHostName", () => {
  it("names a host, never an address", () => {
    expect(turnHostName("turn:edge2.lakko.me:3478?transport=udp")).toBe("edge2.lakko.me");
    expect(turnHostName("TURNS:Edge.Lakko.Me")).toBe("edge.lakko.me");
    expect(turnHostName("turn:46.224.156.97:3478")).toBeNull();
    expect(turnHostName("turn:[2a01:4f8::1]:3478")).toBeNull();
    expect(turnHostName("stun:edge2.lakko.me:3478")).toBeNull();
    expect(turnHostName(null)).toBeNull();
  });
});

describe("CallPathLog", () => {
  it("logs once per path, and again when the path changes", () => {
    const log = new CallPathLog();
    expect(log.note([])).toBeNull();
    expect(log.note(report({}))).toBe("call path: local=host/udp remote=host/udp rtt=12ms");
    expect(log.note(report({ rtt: 0.2 }))).toBeNull();
    expect(log.note(report({ local: { candidateType: "relay", relayProtocol: "udp" } }))).toBe(
      "call path: local=relay/udp/udp remote=host/udp rtt=12ms",
    );
    expect(log.note(report({ pairId: "CP2", local: { candidateType: "relay", relayProtocol: "udp" } }))).not.toBeNull();
  });
});
