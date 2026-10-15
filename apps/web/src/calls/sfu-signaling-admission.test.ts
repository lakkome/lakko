// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The call-admission JWT never rides the connect URL (invariant #5): the
 * socket opens `/v1/connect` with no query and offers the token as a second
 * subprotocol, `lakko-sfu-token.<jwt>`, next to `lakko-sfu-v1`.
 *
 * Headless: a fake WebSocket records what the client opened; the wasm module
 * is the real one.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

vi.mock("../wasm", async () => {
  const real = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => real };
});

import { SfuSignalingClient, sfuSubprotocols } from "./sfu-signaling-client";

const opened: { url: string; protocols: unknown }[] = [];

class FakeWebSocket {
  static OPEN = 1;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: unknown = null;
  onerror: unknown = null;
  onclose: unknown = null;
  constructor(url: string, protocols?: unknown) {
    opened.push({ url, protocols });
    queueMicrotask(() => this.onopen?.());
  }
  send(): void {}
  close(): void {}
}

beforeEach(() => {
  opened.length = 0;
  vi.stubGlobal("WebSocket", FakeWebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("SFU admission", () => {
  it("offers the token as a subprotocol beside lakko-sfu-v1", () => {
    expect(sfuSubprotocols("aa.bb.cc")).toEqual(["lakko-sfu-v1", "lakko-sfu-token.aa.bb.cc"]);
  });

  it("opens /v1/connect with no token in the URL", async () => {
    const jwt = "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ4In0.c2ln";
    const c = new SfuSignalingClient("wss://sfu.test", jwt, {});
    await c.connect();
    expect(opened).toHaveLength(1);
    expect(opened[0]!.url).toBe("wss://sfu.test/v1/connect");
    expect(opened[0]!.url).not.toContain(jwt);
    expect(opened[0]!.protocols).toEqual(["lakko-sfu-v1", `lakko-sfu-token.${jwt}`]);
    c.close();
  });
});
