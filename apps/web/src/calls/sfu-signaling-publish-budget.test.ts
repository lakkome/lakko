// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The signaling client reads the SFU's `publish-budget` frame (ADR-0068,
 * amendment of 2026-10-11).
 *
 * Headless: a fake WebSocket; the wasm module is the real one, so the frame
 * is validated and read by the protocol crate's own code
 * (`validateSfuServerFrame`, `parseSfuPublishBudget`).
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { parseSfuPublishBudget } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

vi.mock("../wasm", async () => {
  const real = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => real };
});

import { SfuSignalingClient, type SfuSignalingCallbacks } from "./sfu-signaling-client";
import { readPublishBudgetFrame, type PublishBudget } from "./publish-budget";
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
  budgets: PublishBudget[];
  parseErrors: string[];
  receive(raw: string): void;
  sent(): string[];
}> {
  const budgets: PublishBudget[] = [];
  const parseErrors: string[] = [];
  const c = new SfuSignalingClient("wss://sfu.test", "jwt", {
    onPublishBudget: (b) => budgets.push(b),
    onParseError: (raw) => parseErrors.push(raw),
    ...extra,
  });
  await c.connect();
  return {
    budgets,
    parseErrors,
    receive: (raw) => sockets[0]?.onmessage?.({ data: raw }),
    sent: () => sockets[0]?.sent ?? [],
  };
}

describe("a publish-budget frame on the signaling socket", () => {
  it("is handed on with both ceilings, each frame the whole state", async () => {
    const c = await client();
    c.receive('{"kind":"publish-budget","main_kbps":1922,"middle_kbps":500}');
    c.receive('{"kind":"publish-budget","main_kbps":200,"middle_kbps":483}');
    c.receive('{"kind":"publish-budget","middle_kbps":500,"main_kbps":20000}');
    expect(c.budgets).toEqual([
      { mainKbps: 1922, middleKbps: 500 },
      { mainKbps: 200, middleKbps: 483 },
      { mainKbps: 20000, middleKbps: 500 },
    ]);
    expect(warn).not.toHaveBeenCalled();
    expect(c.parseErrors).toEqual([]);
    // Reading a frame sends nothing.
    expect(c.sent()).toEqual([]);
  });

  it("that is malformed is ignored with one warning each, and never throws into the socket", async () => {
    const c = await client();
    const malformed = [
      '{"kind":"publish-budget"}',
      '{"kind":"publish-budget","main_kbps":900}',
      '{"kind":"publish-budget","middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":"900","middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":900.5,"middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":-1,"middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":0,"middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":900,"middle_kbps":0}',
      '{"kind":"publish-budget","main_kbps":20001,"middle_kbps":500}',
      '{"kind":"publish-budget","main_kbps":null,"middle_kbps":500}',
    ];
    malformed.forEach((raw, i) => {
      expect(() => c.receive(raw)).not.toThrow();
      expect(warn).toHaveBeenCalledTimes(i + 1);
      expect(String(warn.mock.calls[i]?.[0])).toContain("malformed publish-budget frame");
    });
    expect(c.budgets).toEqual([]);
    expect(c.parseErrors).toHaveLength(malformed.length);
    // A good frame is still read afterwards.
    c.receive('{"kind":"publish-budget","main_kbps":900,"middle_kbps":500}');
    expect(c.budgets).toEqual([{ mainKbps: 900, middleKbps: 500 }]);
  });

  it("is not taken for a video-needed frame, nor the other way round", async () => {
    const needed: VideoNeeded[] = [];
    const c = await client({ onVideoNeeded: (n) => needed.push(n) });
    c.receive('{"kind":"video-needed","main":true,"middle":false}');
    c.receive('{"kind":"publish-budget","main_kbps":900,"middle_kbps":500}');
    expect(needed).toEqual([{ main: true, middle: false }]);
    expect(c.budgets).toEqual([{ mainKbps: 900, middleKbps: 500 }]);
    expect(warn).not.toHaveBeenCalled();
  });

  it("keeps the socket's handler safe from a handler that throws", async () => {
    const c = await client({
      onPublishBudget: () => {
        throw new Error("handler failed");
      },
    });
    expect(() => c.receive('{"kind":"publish-budget","main_kbps":900,"middle_kbps":500}')).not.toThrow();
  });

  it("with nobody listening is simply dropped", async () => {
    const c = new SfuSignalingClient("wss://sfu.test", "jwt", {});
    await c.connect();
    expect(() =>
      sockets[0]?.onmessage?.({ data: '{"kind":"publish-budget","main_kbps":900,"middle_kbps":500}' }),
    ).not.toThrow();
    expect(warn).not.toHaveBeenCalled();
  });
});

describe("the shared parser", () => {
  it("returns the two ceilings as a Uint32Array [main_kbps, middle_kbps]", () => {
    const out = parseSfuPublishBudget('{"kind":"publish-budget","main_kbps":2500,"middle_kbps":500}');
    expect(out).toBeInstanceOf(Uint32Array);
    expect([...out]).toEqual([2500, 500]);
    expect(() => parseSfuPublishBudget('{"kind":"pong","nonce":1}')).toThrow();
  });

  it("is what the reader reads through", () => {
    const wasm = { parseSfuPublishBudget };
    expect(
      readPublishBudgetFrame(wasm, '{"kind":"publish-budget","main_kbps":1,"middle_kbps":20000}'),
    ).toEqual({ mainKbps: 1, middleKbps: 20000 });
    expect(readPublishBudgetFrame(wasm, '{"kind":"publish-budget","main_kbps":0,"middle_kbps":1}')).toBeNull();
    expect(readPublishBudgetFrame(wasm, "not json")).toBeNull();
  });
});
