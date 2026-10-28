// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  SFU_CALL_STATE_PROBE_MS,
  parseSfuCallState,
  sfuRoomStateUrl,
} from "./sfu-call-state";

describe("sfuRoomStateUrl", () => {
  it("asks the HTTP origin behind the WebSocket base for the room, no token anywhere", () => {
    expect(sfuRoomStateUrl("wss://test.lakko.me/sfu", "ab".repeat(32))).toBe(
      `https://test.lakko.me/sfu/v1/rooms/${"ab".repeat(32)}`,
    );
    expect(sfuRoomStateUrl("ws://127.0.0.1:9640/", "cd".repeat(32))).toBe(
      `http://127.0.0.1:9640/v1/rooms/${"cd".repeat(32)}`,
    );
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
