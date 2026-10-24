// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { iceServersFor, iceServersLine, stunUrlsFromTurn } from "./ice-servers";

describe("stunUrlsFromTurn", () => {
  it("takes the host and port of each turn: URI, without its transport", () => {
    expect(
      stunUrlsFromTurn(["turn:edge2.lakko.me:3478?transport=tcp", "turn:edge2.lakko.me:3478?transport=udp"]),
    ).toEqual(["stun:edge2.lakko.me:3478"]);
  });

  it("uses 3478 when the URI names no port, and keeps an IPv6 host bracketed", () => {
    expect(stunUrlsFromTurn("turn:turn.example")).toEqual(["stun:turn.example:3478"]);
    expect(stunUrlsFromTurn("turn:[2001:db8::1]:3479")).toEqual(["stun:[2001:db8::1]:3479"]);
  });

  it("gives nothing for turns: and for what does not parse", () => {
    expect(stunUrlsFromTurn(["turns:edge2.lakko.me:5349?transport=tcp", "turn:", "turn:host:99999", "turn:[::1"])).toEqual(
      [],
    );
  });
});

describe("iceServersFor", () => {
  it("is empty without a TURN entry: no public STUN server", () => {
    expect(iceServersFor(null)).toEqual([]);
  });

  it("is the TURN entry and STUN on its host, and nothing on google.com", () => {
    const turn = { urls: ["turn:edge2.lakko.me:3478?transport=tcp"], username: "u", credential: "c" };
    const servers = iceServersFor(turn);
    expect(servers).toEqual([{ urls: ["stun:edge2.lakko.me:3478"] }, turn]);
    expect(JSON.stringify(servers)).not.toContain("google");
  });

  it("logs the URLs and never the credential", () => {
    const line = iceServersLine(iceServersFor({ urls: "turn:edge2.lakko.me:3478", username: "user-x", credential: "secret-y" }));
    expect(line).toBe("stun:edge2.lakko.me:3478, turn:edge2.lakko.me:3478");
    expect(line).not.toContain("user-x");
    expect(line).not.toContain("secret-y");
    expect(iceServersLine([])).toBe("none (host candidates only)");
  });
});
