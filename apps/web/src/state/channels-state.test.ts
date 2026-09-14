// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it, vi } from "vitest";

// The regression this pins only reproduces when `localStorage` EXISTS at module
// evaluation, which the plain `node` test environment does not provide -- that
// absence is exactly why the blank-page crash shipped. So the store is stubbed
// before the module is imported, and the import is dynamic so the stub wins.
function stubLocalStorage(seed: Record<string, string>): void {
  const map = new Map(Object.entries(seed));
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
  });
}

describe("channels-state module evaluation", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("imports without a temporal-dead-zone crash when localStorage exists", async () => {
    stubLocalStorage({});
    await expect(import("./channels-state")).resolves.toBeDefined();
  });

  it("loads persisted subscriber keys at evaluation, which is the path that read the key early", async () => {
    const secretHex = "11".repeat(32);
    const publicHex = "22".repeat(32);
    stubLocalStorage({
      "lakko.channels.subscriber-keys.v1": JSON.stringify([
        { channelId: "chan-a", secretHex, publicHex },
      ]),
    });
    const { channelsState } = await import("./channels-state");
    const keys = channelsState.subscriberKeysByChannel().get("chan-a");
    expect(keys?.public).toEqual(new Uint8Array(32).fill(0x22));
  });
});
