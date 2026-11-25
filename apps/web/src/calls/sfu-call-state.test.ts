// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  SFU_CALL_STATE_PROBE_MS,
  fetchSfuRoomState,
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

describe("fetchSfuRoomState", () => {
  const room = "cd".repeat(32);
  const answer = (status: number, body: unknown): typeof fetch =>
    (async () =>
      new Response(JSON.stringify(body), { status })) as unknown as typeof fetch;

  it("returns the room's state, asked with no credentials", async () => {
    let seen: { url: string; init: RequestInit | undefined } | null = null;
    const impl = (async (url: string, init?: RequestInit) => {
      seen = { url, init };
      return new Response(JSON.stringify({ state: "ended" }), { status: 200 });
    }) as unknown as typeof fetch;
    expect(await fetchSfuRoomState("wss://sfu.test", room, impl)).toBe("ended");
    expect(seen).not.toBeNull();
    expect(seen!.url).toBe(`https://sfu.test/v1/rooms/${room}`);
    expect(seen!.init?.credentials).toBe("omit");
  });

  it("returns null for a non-2xx answer, an unreadable body or a network failure", async () => {
    expect(await fetchSfuRoomState("wss://sfu.test", room, answer(502, { state: "ended" }))).toBeNull();
    expect(await fetchSfuRoomState("wss://sfu.test", room, answer(200, { state: "gone" }))).toBeNull();
    const failing = (async () => {
      throw new Error("offline");
    }) as unknown as typeof fetch;
    expect(await fetchSfuRoomState("wss://sfu.test", room, failing)).toBeNull();
  });
});
