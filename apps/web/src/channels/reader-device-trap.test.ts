// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The trap rule (2026-11-13, contract section 6): a device of a channel's
 * owner that holds no PublisherGroup for it never founds one and never claims
 * or resets the channel's journal; only New channel founds. The groups are the
 * REAL core; the Delivery Service is a fetch spy.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

vi.mock("../wasm", async () => {
  const m = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => m, wasmSync: () => m };
});

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  openHeldPublisherGroup,
  openOrFoundPublisherGroup,
  PublisherGroupNotHeldError,
  type UnconfirmedFoundingStorage,
} from "./composer";
import { openPublisherGroup } from "./copublisher";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function mem(): UnconfirmedFoundingStorage & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, v),
    removeItem: (k) => void data.delete(k),
  };
}

const ACCOUNT = "Y2FyYS1hY2NvdW50LTAwMDA=";

function hex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

describe("a device with no PublisherGroup for a channel founds nothing", () => {
  it("openHeldPublisherGroup refuses, makes no request and leaves no group behind", async () => {
    // The owner's second device: the same account seed, no group state.
    const reader = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x71));
    const channelId = new Uint8Array(16).fill(0x72);
    const st = mem();
    const ds = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", ds);
    try {
      await expect(openHeldPublisherGroup(wasm, reader, channelId, st, ACCOUNT)).rejects.toBeInstanceOf(
        PublisherGroupNotHeldError,
      );
      expect(ds).not.toHaveBeenCalled();
      expect(st.data.size).toBe(0);
      expect(wasm.PublisherGroup.load(reader, channelId)).toBeUndefined();
    } finally {
      reader.free();
    }
  });

  it("every path but New channel (openPublisherGroup) refuses the same way", async () => {
    const reader = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x73));
    const channelId = new Uint8Array(16).fill(0x74);
    const ds = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", ds);
    try {
      await expect(openPublisherGroup(reader, hex(channelId))).rejects.toBeInstanceOf(PublisherGroupNotHeldError);
      expect(ds).not.toHaveBeenCalled();
      expect(wasm.PublisherGroup.load(reader, channelId)).toBeUndefined();
    } finally {
      reader.free();
    }
  });

  it("the device that founded the group (New channel) opens it, claiming nothing again", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x75));
    const channelId = new Uint8Array(16).fill(0x76);
    const st = mem();
    try {
      vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })));
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      const later = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", later);
      const held = await openHeldPublisherGroup(wasm, owner, channelId, st, ACCOUNT);
      expect(later).not.toHaveBeenCalled();
      held.free();
    } finally {
      owner.free();
      warn.mockRestore();
    }
  });

  it("a held group whose founding claim never landed claims again, as before", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x77));
    const channelId = new Uint8Array(16).fill(0x78);
    const st = mem();
    try {
      vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 500 })));
      await expect(openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).rejects.toBeDefined();
      expect(st.data.size).toBe(1);
      const ok = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", ok);
      (await openHeldPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      expect(ok).toHaveBeenCalledTimes(1);
      expect(st.data.size).toBe(0);
    } finally {
      owner.free();
      warn.mockRestore();
    }
  });
});
