// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The signaling client reads the SFU's `video-needed` frame (ADR-0070,
 * "senders nobody watches").
 *
 * Headless: a fake WebSocket; the wasm module is the real one, so the frame
 * is validated and read by the protocol crate's own code
 * (`validateSfuServerFrame`, `parseSfuVideoNeeded`).
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
import type { VideoNeeded } from "./sender-activity";

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
  needed: VideoNeeded[];
  parseErrors: string[];
  receive(raw: string): void;
  sent(): string[];
}> {
  const needed: VideoNeeded[] = [];
  const parseErrors: string[] = [];
  const c = new SfuSignalingClient("wss://sfu.test", "jwt", {
    onVideoNeeded: (n) => needed.push(n),
    onParseError: (raw) => parseErrors.push(raw),
    ...extra,
  });
  await c.connect();
  return {
    needed,
    parseErrors,
    receive: (raw) => sockets[0]?.onmessage?.({ data: raw }),
    sent: () => sockets[0]?.sent ?? [],
  };
}

describe("a video-needed frame on the signaling socket", () => {
  it("is handed on with both flags, each frame the whole state", async () => {
    const c = await client();
    c.receive('{"kind":"video-needed","main":true,"middle":true}');
    c.receive('{"kind":"video-needed","main":false,"middle":true}');
    c.receive('{"kind":"video-needed","main":false,"middle":true}');
    c.receive('{"kind":"video-needed","middle":false,"main":true}');
    expect(c.needed).toEqual([
      { main: true, middle: true },
      { main: false, middle: true },
      { main: false, middle: true },
      { main: true, middle: false },
    ]);
    expect(warn).not.toHaveBeenCalled();
    expect(c.parseErrors).toEqual([]);
    // Reading a frame sends nothing.
    expect(c.sent()).toEqual([]);
  });

  it("that is malformed is ignored with one warning each, and never throws into the socket", async () => {
    const c = await client();
    const malformed = [
      '{"kind":"video-needed"}',
      '{"kind":"video-needed","main":true}',
      '{"kind":"video-needed","middle":false}',
      '{"kind":"video-needed","main":1,"middle":0}',
      '{"kind":"video-needed","main":"true","middle":"false"}',
      '{"kind":"video-needed","main":null,"middle":true}',
    ];
    malformed.forEach((raw, i) => {
      expect(() => c.receive(raw)).not.toThrow();
      expect(warn).toHaveBeenCalledTimes(i + 1);
      expect(String(warn.mock.calls[i]?.[0])).toContain("malformed video-needed frame");
    });
    expect(c.needed).toEqual([]);
    expect(c.parseErrors).toHaveLength(malformed.length);
    // A good frame is still read afterwards.
    c.receive('{"kind":"video-needed","main":false,"middle":false}');
    expect(c.needed).toEqual([{ main: false, middle: false }]);
  });

  it("does not make another refused frame warn", async () => {
    const c = await client();
    c.receive("not json");
    c.receive(JSON.stringify({ kind: "no-such-kind" }));
    expect(warn).not.toHaveBeenCalled();
    expect(c.parseErrors).toHaveLength(2);
  });

  it("is not taken for a video-given frame, nor the other way round", async () => {
    const given: unknown[] = [];
    const c = await client({ onVideoGiven: (g) => given.push(g) });
    c.receive('{"kind":"video-needed","main":true,"middle":false}');
    c.receive('{"kind":"video-given","given":[]}');
    expect(c.needed).toEqual([{ main: true, middle: false }]);
    expect(given).toEqual([[]]);
  });

  it("keeps the socket's handler safe from a handler that throws", async () => {
    const c = await client({
      onVideoNeeded: () => {
        throw new Error("handler failed");
      },
    });
    expect(() => c.receive('{"kind":"video-needed","main":true,"middle":true}')).not.toThrow();
  });

  it("with nobody listening is simply dropped", async () => {
    const c = new SfuSignalingClient("wss://sfu.test", "jwt", {});
    await c.connect();
    expect(() =>
      sockets[0]?.onmessage?.({ data: '{"kind":"video-needed","main":false,"middle":false}' }),
    ).not.toThrow();
    expect(warn).not.toHaveBeenCalled();
  });
});
