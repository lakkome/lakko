// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { channelPollVoteMessage } from "../../wasm/lakko_ffi_wasm";
import { nextVoteAtMs, optionCountsOf, VOTE_WINDOW_MS, voteAnswerOf, voteMessageBytes } from "./channel-poll";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

describe("the channel vote retry (ADR-0090 section 4.3)", () => {
  it("retries a stale vote once at stored_at_ms + 1 inside the window", async () => {
    const a = await voteAnswerOf(json(409, { code: "stale-vote", stored_at_ms: 5_000 }));
    expect(a).toEqual({ kind: "stale", storedAtMs: 5_000 });
    expect(nextVoteAtMs(a, 0, 5_100)).toBe(5_001);
    expect(nextVoteAtMs(a, 1, 5_100)).toBeNull();
  });

  it("gives up on a stale vote whose next time is outside the window", () => {
    expect(nextVoteAtMs({ kind: "stale", storedAtMs: 0 }, 0, VOTE_WINDOW_MS + 10)).toBeNull();
  });

  it("retries once at the service's clock when the time is out of window", async () => {
    const a = await voteAnswerOf(json(400, { code: "vote-time-out-of-window", server_time_ms: 77 }));
    expect(nextVoteAtMs(a, 0, 1_000_000)).toBe(77);
  });

  it("does not retry other refusals", async () => {
    const a = await voteAnswerOf(json(429, { code: "vote-rate-limited" }));
    expect(a).toEqual({ kind: "failed", code: "vote-rate-limited" });
    expect(nextVoteAtMs(a, 0, 0)).toBeNull();
    expect(await voteAnswerOf(new Response(null, { status: 201 }))).toEqual({ kind: "ok" });
  });

  it("builds the same message the core binds", () => {
    const l1 = new Uint8Array(16).fill(9);
    const l2 = new Uint8Array(16).fill(1);
    const flat = new Uint8Array(32);
    flat.set(l1, 0);
    flat.set(l2, 16);
    expect(Array.from(voteMessageBytes("put", 1234, [l1, l2]))).toEqual(
      Array.from(channelPollVoteMessage("put", 1234n, flat)),
    );
    expect(Array.from(voteMessageBytes("delete", 99, []))).toEqual(
      Array.from(channelPollVoteMessage("delete", 99n, new Uint8Array(0))),
    );
  });

  it("reads counts per option from the label counts", () => {
    expect(optionCountsOf(["AA", "bb", "cc"], new Map([["aa", 3], ["cc", 1]]))).toEqual([3, 0, 1]);
  });
});
