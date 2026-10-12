// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The suite-wide network guard (`no-network.ts`) guards. */
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchIsGuarded, takeRefusedRequests } from "./no-network";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the network guard", () => {
  it("is installed before any test of a file runs", () => {
    expect(fetchIsGuarded()).toBe(true);
  });

  it("fetch refuses, names the URL, and the request is recorded", async () => {
    await expect(fetch("https://test.lakko.me/api/ds/ds/group/aa/messages")).rejects.toThrow(
      "[no-network]",
    );
    await expect(fetch(new URL("https://example.org/x"))).rejects.toThrow("https://example.org/x");
    // Taken here: left in place, the guard's own afterEach fails this test,
    // which is the behaviour the next test relies on.
    expect(takeRefusedRequests()).toEqual([
      "fetch https://test.lakko.me/api/ds/ds/group/aa/messages",
      "fetch https://example.org/x",
    ]);
  });

  it("a request the code under test SWALLOWED is still recorded", async () => {
    const bestEffort = async (): Promise<string> => {
      try {
        await fetch("https://test.lakko.me/api/push/register");
        return "sent";
      } catch {
        return "ignored";
      }
    };
    await expect(bestEffort()).resolves.toBe("ignored");
    expect(takeRefusedRequests()).toEqual(["fetch https://test.lakko.me/api/push/register"]);
  });

  it("WebSocket, EventSource and XMLHttpRequest refuse", () => {
    const g = globalThis as unknown as Record<string, new (...a: unknown[]) => { open?: (...a: unknown[]) => void }>;
    expect(() => new g["WebSocket"]!("wss://sfu.lakko.me/v1/connect")).toThrow("[no-network]");
    expect(() => new g["EventSource"]!("https://example.org/events")).toThrow("[no-network]");
    const xhr = new g["XMLHttpRequest"]!();
    expect(() => xhr.open?.("GET", "https://example.org/")).toThrow("[no-network]");
    expect(takeRefusedRequests()).toHaveLength(3);
  });

  it("a test that installs its own transport is not refused, and gets the guard back afterwards", async () => {
    vi.stubGlobal("fetch", async () => new Response("ok"));
    expect(fetchIsGuarded()).toBe(false);
    await expect(fetch("https://example.org/").then((r) => r.text())).resolves.toBe("ok");
    vi.unstubAllGlobals();
    expect(fetchIsGuarded()).toBe(true);
    expect(takeRefusedRequests()).toEqual([]);
  });
});
