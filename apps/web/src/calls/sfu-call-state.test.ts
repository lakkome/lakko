// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { SFU_CALL_STATE_PROBE_MS, parseSfuCallState, sfuCallStateUrl } from "./sfu-call-state";

describe("sfuCallStateUrl", () => {
  it("asks the HTTP origin behind the WebSocket base, token in the query", () => {
    expect(sfuCallStateUrl("wss://test.lakko.me/sfu", "ab".repeat(16), "a.b.c")).toBe(
      `https://test.lakko.me/sfu/v1/calls/${"ab".repeat(16)}?token=a.b.c`,
    );
    expect(sfuCallStateUrl("ws://127.0.0.1:9640/", "cd".repeat(16), "t")).toBe(
      `http://127.0.0.1:9640/v1/calls/${"cd".repeat(16)}?token=t`,
    );
  });

  it("escapes the token", () => {
    expect(sfuCallStateUrl("wss://h/sfu", "00".repeat(16), "a b&c")).toContain("token=a%20b%26c");
  });
});

describe("parseSfuCallState", () => {
  it("reads the four words", () => {
    for (const s of ["unknown", "live", "closing", "ended"] as const) {
      expect(parseSfuCallState({ state: s, peers: 0 })).toBe(s);
    }
  });

  it("reads anything else as no answer, never as ended", () => {
    expect(parseSfuCallState({ state: "gone" })).toBeNull();
    expect(parseSfuCallState({})).toBeNull();
    expect(parseSfuCallState("ended")).toBeNull();
    expect(parseSfuCallState(null)).toBeNull();
  });

  it("probes slower than the SFU's own grace", () => {
    expect(SFU_CALL_STATE_PROBE_MS).toBe(10_000);
  });
});
