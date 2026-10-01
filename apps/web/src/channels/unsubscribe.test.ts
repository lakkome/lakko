// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - leaving a channel: the request `unsubscribeSubscriber`
 * sends, and how `leaveChannelOnServer` signs it (REAL wasm key pair) and
 * reads the answer. `404` counts as done; a failure is an outcome, never a
 * throw.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import { unsubscribeSubscriber } from "./api";
import { bytesToHex, type ChannelSubscriberKeys } from "./composer";
import { leaveChannelOnServer } from "./unsubscribe";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const CH = "00112233445566778899aabbccddeeff";
const NOW = 1_790_000_000_123;
const realFetch = globalThis.fetch;

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.restoreAllMocks();
});

interface Captured {
  readonly url: string;
  readonly init: RequestInit | undefined;
}

function installFetch(respond: () => Response | Promise<Response>): Captured[] {
  const calls: Captured[] = [];
  (globalThis as unknown as { fetch: typeof fetch }).fetch = vi.fn(
    (input: string | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return Promise.resolve(respond());
    },
  ) as unknown as typeof fetch;
  return calls;
}

function keys(): ChannelSubscriberKeys {
  const kp = wasm.ChannelSubscriberKeypair.generate();
  try {
    return { secret: kp.secret(), public: kp.public() };
  } finally {
    kp.free();
  }
}

const jsonError = (status: number, code: string): Response =>
  new Response(JSON.stringify({ code, detail: code }), {
    status,
    headers: { "content-type": "application/json" },
  });

const W = wasm as unknown as Wasm;

describe("unsubscribeSubscriber", () => {
  it("sends a DELETE on the subscriber path with the signature header and no body", async () => {
    const calls = installFetch(() => new Response(null, { status: 204 }));
    const pk = "ab".repeat(32);
    await unsubscribeSubscriber(CH, pk, { headerName: "x-sig", value: "1.c2ln" });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/subscribers/${pk}`);
    expect(calls[0]!.init?.method).toBe("DELETE");
    expect(calls[0]!.init?.body).toBeUndefined();
    expect((calls[0]!.init?.headers as Record<string, string>)["x-sig"]).toBe("1.c2ln");
  });
});

describe("leaveChannelOnServer", () => {
  it("signs with the subscriber key under the core's header name and stamp", async () => {
    const calls = installFetch(() => new Response(null, { status: 204 }));
    const k = keys();
    const out = await leaveChannelOnServer(CH, k, { wasm: W, now: () => NOW });
    expect(out).toEqual({ kind: "left" });
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/subscribers/${bytesToHex(k.public)}`);
    const headers = calls[0]!.init?.headers as Record<string, string>;
    const name = wasm.unsubscribeSigHeaderName();
    expect(name).toBe("x-lakko-unsubscribe-sig");
    const value = headers[name]!;
    const [stamp, sig] = value.split(".");
    expect(stamp).toBe(String(NOW));
    // A 64-byte Ed25519 signature, base64url without padding.
    expect(sig).toMatch(/^[A-Za-z0-9_-]{86}$/);
    // The same header the core composes for this key, channel and moment.
    const kp = new wasm.ChannelSubscriberKeypair(k.secret);
    try {
      const id = new Uint8Array(16);
      for (let i = 0; i < 16; i++) id[i] = Number.parseInt(CH.slice(i * 2, i * 2 + 2), 16);
      expect(value).toBe(kp.unsubscribeRequestHeader(id, BigInt(NOW)));
    } finally {
      kp.free();
    }
  });

  it("counts a 404 from the service as done", async () => {
    installFetch(() => jsonError(404, "unknown-channel"));
    expect(await leaveChannelOnServer(CH, keys(), { wasm: W, now: () => NOW })).toEqual({ kind: "left" });
  });

  it("reports a network failure without throwing", async () => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi.fn(() =>
      Promise.reject(new TypeError("Failed to fetch")),
    ) as unknown as typeof fetch;
    expect(await leaveChannelOnServer(CH, keys(), { wasm: W, now: () => NOW })).toEqual({ kind: "network" });
  });

  it("reports a refusal with its status and code", async () => {
    installFetch(() => jsonError(401, "stale-unsubscribe"));
    expect(await leaveChannelOnServer(CH, keys(), { wasm: W, now: () => NOW })).toEqual({
      kind: "refused",
      status: 401,
      code: "stale-unsubscribe",
    });
  });

  it("does not send a request it could not sign", async () => {
    const calls = installFetch(() => new Response(null, { status: 204 }));
    const out = await leaveChannelOnServer("not-a-channel", keys(), { wasm: W, now: () => NOW });
    expect(out).toEqual({ kind: "unsigned" });
    expect(calls).toHaveLength(0);
  });
});
