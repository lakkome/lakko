// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 10 - `TransparencyClient.fetchAnnouncement`: the route,
 * its encodings and its `404`, as `services/transparency` serves them
 * (`GET /transparency/announcement/{username_hash}/{generation}`).
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { TransparencyClient, TransparencyHttpError, TransparencyNetworkError } from "./client";

const ACCOUNT_KEY = new Uint8Array(32).fill(0xfb);

function stubFetch(answer: () => Response | Promise<Response>): ReturnType<typeof vi.fn> {
  const f = vi.fn(async (_url: string, _init?: RequestInit) => answer());
  vi.stubGlobal("fetch", f);
  return f;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("TransparencyClient.fetchAnnouncement", () => {
  it("asks for the account key URL-safe without padding and the generation in decimal", async () => {
    const f = stubFetch(
      () =>
        new Response(
          JSON.stringify({ idx: 12, key_generation: 3, seq: 41, announcement_b64: btoa("announcement") }),
          { status: 200 },
        ),
    );
    const entry = await new TransparencyClient("https://log.example").fetchAnnouncement(ACCOUNT_KEY, 3n);
    expect(f).toHaveBeenCalledTimes(1);
    const url = f.mock.calls[0]?.[0] as string;
    // 0xfb bytes encode with `-` and `_` in the URL-safe alphabet, no `=`.
    expect(url).toMatch(/^https:\/\/log\.example\/transparency\/announcement\/[A-Za-z0-9_-]{43}\/3$/);
    expect(url).not.toContain("=");
    expect(entry).toEqual({
      idx: 12n,
      keyGeneration: 3n,
      seq: 41n,
      announcement: new TextEncoder().encode("announcement"),
    });
  });

  it("404 is `null`: the account logged no announcement at that generation", async () => {
    stubFetch(() => new Response("no announcement for that generation", { status: 404 }));
    expect(await new TransparencyClient("https://log.example").fetchAnnouncement(ACCOUNT_KEY, 1n)).toBeNull();
  });

  it("a body of another shape and a server error are errors, never an entry", async () => {
    stubFetch(() => new Response(JSON.stringify({ idx: "12" }), { status: 200 }));
    await expect(
      new TransparencyClient("https://log.example").fetchAnnouncement(ACCOUNT_KEY, 1n),
    ).rejects.toBeInstanceOf(TransparencyHttpError);
    stubFetch(() => new Response("boom", { status: 500 }));
    await expect(
      new TransparencyClient("https://log.example").fetchAnnouncement(ACCOUNT_KEY, 1n),
    ).rejects.toBeInstanceOf(TransparencyHttpError);
  });

  it("no answer is a network error", async () => {
    stubFetch(() => Promise.reject(new TypeError("failed to fetch")));
    await expect(
      new TransparencyClient("https://log.example").fetchAnnouncement(ACCOUNT_KEY, 1n),
    ).rejects.toBeInstanceOf(TransparencyNetworkError);
  });
});
