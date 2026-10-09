// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The signaling client reads the SFU's `video-given` frame (ADR-0070 step 6).
 *
 * Headless: a fake WebSocket; the wasm module is the real one, so the frame
 * is validated and read by the protocol crate's own code
 * (`validateSfuServerFrame`, `parseSfuVideoGiven`).
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

import { SfuSignalingClient, type SfuSignalingCallbacks } from "./sfu-signaling-client";
import type { VideoGivenEntry } from "./video-given";

const sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static OPEN = 1;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onerror: unknown = null;
  onclose: unknown = null;
  sent: string[] = [];
  constructor() {
    sockets.push(this);
    queueMicrotask(() => this.onopen?.());
  }
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {}
}

let warn: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  sockets.length = 0;
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.stubGlobal("WebSocket", FakeWebSocket);
});

afterEach(() => {
  warn.mockRestore();
  vi.unstubAllGlobals();
});

async function client(extra: SfuSignalingCallbacks = {}): Promise<{
  given: Array<readonly VideoGivenEntry[]>;
  parseErrors: string[];
  receive(raw: string): void;
  sent(): string[];
}> {
  const given: Array<readonly VideoGivenEntry[]> = [];
  const parseErrors: string[] = [];
  const c = new SfuSignalingClient("wss://sfu.test", "jwt", {
    onVideoGiven: (g) => given.push(g),
    onParseError: (raw) => parseErrors.push(raw),
    ...extra,
  });
  await c.connect();
  return {
    given,
    parseErrors,
    receive: (raw) => sockets[0]?.onmessage?.({ data: raw }),
    sent: () => sockets[0]?.sent ?? [],
  };
}

const frame = (given: unknown): string => JSON.stringify({ kind: "video-given", given });

describe("a video-given frame on the signaling socket", () => {
  it("is handed on as the whole set, in the frame's order", async () => {
    const c = await client();
    c.receive(
      '{"kind":"video-given","given":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}',
    );
    expect(c.given).toEqual([
      [
        { peer: "p1", quality: "main" },
        { peer: "p2", quality: "middle" },
        { peer: "p3", quality: "light" },
      ],
    ]);
    c.receive(frame([]));
    expect(c.given.at(-1)).toEqual([]);
    expect(warn).not.toHaveBeenCalled();
    expect(c.parseErrors).toEqual([]);
    // Reading a frame sends nothing.
    expect(c.sent()).toEqual([]);
  });

  it("with an unknown quality is ignored with one warning", async () => {
    const c = await client();
    c.receive(frame([{ peer: "p1", quality: "main" }, { peer: "p2", quality: "full" }]));
    expect(c.given).toEqual([]);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("video-given");
  });

  it("that is malformed is ignored with one warning each, and never throws into the socket", async () => {
    const c = await client();
    const malformed = [
      frame("main"),
      frame([{ peer: "p1" }]),
      frame([{ peer: "", quality: "main" }]),
      JSON.stringify({ kind: "video-given" }),
      frame(Array.from({ length: 65 }, (_, i) => ({ peer: `p${i}`, quality: "main" }))),
    ];
    malformed.forEach((raw, i) => {
      expect(() => c.receive(raw)).not.toThrow();
      expect(warn).toHaveBeenCalledTimes(i + 1);
    });
    expect(c.given).toEqual([]);
    // A good frame is still read afterwards.
    c.receive(frame([{ peer: "p1", quality: "light" }]));
    expect(c.given).toEqual([[{ peer: "p1", quality: "light" }]]);
  });

  it("does not make another refused frame warn", async () => {
    const c = await client();
    c.receive("not json");
    c.receive(JSON.stringify({ kind: "no-such-kind" }));
    expect(warn).not.toHaveBeenCalled();
    expect(c.parseErrors).toHaveLength(2);
  });

  it("whose handler throws does not break the socket's handler", async () => {
    const pongs: number[] = [];
    const c = await client({
      onVideoGiven: () => {
        throw new Error("handler failed");
      },
      onPong: (f) => pongs.push(f.nonce),
    });
    expect(() => c.receive(frame([{ peer: "p1", quality: "main" }]))).not.toThrow();
    c.receive(JSON.stringify({ kind: "pong", nonce: 7 }));
    expect(pongs).toEqual([7]);
  });
});
