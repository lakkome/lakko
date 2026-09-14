// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §4 — the GIF proxy client: URL shape under the `/api/gifs` prefix,
 * the bearer token, the failure mapping the picker's states hang off, the
 * refusal to follow any URL that is not a proxy media path, and the debounce.
 */
import { describe, expect, it, vi } from "vitest";

import {
  GIF_API_BASE,
  GIF_SEARCH_DEBOUNCE_MS,
  MAX_GIF_FULL_BYTES,
  createDebouncer,
  fetchGifMedia,
  fetchGifPage,
  gifListUrl,
  gifMediaUrl,
  parseGifPage,
  type FetchFn,
  type Timers,
} from "./gif-client";

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function capture(responder: (url: string) => Response): { fetchFn: FetchFn; calls: { url: string; init?: RequestInit }[] } {
  const calls: { url: string; init?: RequestInit }[] = [];
  const fetchFn: FetchFn = (url, init) => {
    calls.push(init === undefined ? { url } : { url, init });
    return Promise.resolve(responder(url));
  };
  return { fetchFn, calls };
}

const ITEM = { id: "abc123", preview_url: "/gifs/media/abc123/preview", full_url: "/gifs/media/abc123/full", width: 480, height: 270 };

describe("URLs", () => {
  it("builds trending and search URLs under the web prefix", () => {
    expect(gifListUrl("", 0)).toBe("/api/gifs/gifs/trending?offset=0");
    expect(gifListUrl("   ", 24)).toBe("/api/gifs/gifs/trending?offset=24");
    expect(gifListUrl(" happy cat ", 48)).toBe("/api/gifs/gifs/search?q=happy%20cat&offset=48");
    expect(gifListUrl("a&offset=9", -3)).toBe("/api/gifs/gifs/search?q=a%26offset%3D9&offset=0");
  });

  it("only ever follows a relative proxy media path", () => {
    expect(gifMediaUrl("/gifs/media/abc123/preview")).toBe(`${GIF_API_BASE}/gifs/media/abc123/preview`);
    for (const bad of [
      "https://media.giphy.com/media/abc/giphy.gif",
      "//evil.example/gifs/media/abc/full",
      "/gifs/media/../../auth/x/full",
      "/gifs/media/abc123/full?u=https://x",
      "/gifs/media/abc123/original",
      "gifs/media/abc123/full",
      42,
      null,
    ]) {
      expect(gifMediaUrl(bad), String(bad)).toBeNull();
    }
  });

  it("drops items outside the contract and keeps the rest", () => {
    const page = parseGifPage({
      items: [ITEM, { ...ITEM, preview_url: "https://media.giphy.com/x.gif" }, { ...ITEM, width: 0 }, { ...ITEM, height: 5000 }, "nope"],
      next_offset: 24,
    });
    expect(page?.items.map((i) => i.id)).toEqual(["abc123"]);
    expect(page?.items[0]?.previewUrl).toBe("/api/gifs/gifs/media/abc123/preview");
    expect(page?.nextOffset).toBe(24);
    expect(parseGifPage({ items: [], next_offset: null })?.nextOffset).toBeNull();
    expect(parseGifPage({ nope: true })).toBeNull();
  });
});

describe("list requests", () => {
  it("sends the bearer token and returns the page", async () => {
    const { fetchFn, calls } = capture(() => json(200, { items: [ITEM], next_offset: null }));
    const r = await fetchGifPage("cats", 0, "tok-1", fetchFn);
    expect(r.kind).toBe("ok");
    expect(calls[0]?.url).toBe("/api/gifs/gifs/search?q=cats&offset=0");
    expect((calls[0]?.init?.headers as Record<string, string>).authorization).toBe("Bearer tok-1");
  });

  it("503 with the proxy's code means not configured (the tab hides)", async () => {
    const { fetchFn } = capture(() => json(503, { error: "gif-provider-not-configured" }));
    expect(await fetchGifPage("", 0, "t", fetchFn)).toEqual({ kind: "not-configured" });
  });

  it("a 503 that is not the proxy's own answer is an outage, not a hidden tab", async () => {
    const { fetchFn } = capture(() => new Response("<html>bad gateway</html>", { status: 503 }));
    expect(await fetchGifPage("", 0, "t", fetchFn)).toEqual({ kind: "unavailable" });
  });

  it("429 carries Retry-After", async () => {
    const { fetchFn } = capture(() => json(429, { error: "rate-limited" }, { "retry-after": "7" }));
    expect(await fetchGifPage("cats", 0, "t", fetchFn)).toEqual({ kind: "rate-limited", retryAfterSecs: 7 });
    const { fetchFn: noHeader } = capture(() => json(429, { error: "rate-limited" }));
    expect(await fetchGifPage("cats", 0, "t", noHeader)).toEqual({ kind: "rate-limited", retryAfterSecs: 1 });
  });

  it("502, 401, a network error and a malformed body map to their states", async () => {
    expect(await fetchGifPage("", 0, "t", capture(() => json(502, { error: "gif-upstream-failed" })).fetchFn)).toEqual({ kind: "unavailable" });
    expect(await fetchGifPage("", 0, "t", capture(() => new Response("", { status: 401 })).fetchFn)).toEqual({ kind: "unauthorized" });
    expect(await fetchGifPage("", 0, "t", () => Promise.reject(new Error("offline")))).toEqual({ kind: "unavailable" });
    expect(await fetchGifPage("", 0, "t", capture(() => json(200, { items: "x" })).fetchFn)).toEqual({ kind: "unavailable" });
  });
});

describe("media requests", () => {
  const url = "/api/gifs/gifs/media/abc123/full";

  it("returns bytes and the response type", async () => {
    const { fetchFn, calls } = capture(() => new Response(new Uint8Array([1, 2, 3]), { status: 200, headers: { "content-type": "image/gif" } }));
    const r = await fetchGifMedia(url, "tok", MAX_GIF_FULL_BYTES, fetchFn);
    expect(r).toEqual({ kind: "ok", value: { bytes: new Uint8Array([1, 2, 3]), mime: "image/gif" } });
    expect((calls[0]?.init?.headers as Record<string, string>).authorization).toBe("Bearer tok");
  });

  it("refuses a type outside the proxy's set, an oversize body and a foreign URL", async () => {
    const html = capture(() => new Response("<script>", { status: 200, headers: { "content-type": "text/html" } }));
    expect(await fetchGifMedia(url, "t", 100, html.fetchFn)).toEqual({ kind: "unavailable" });
    const big = capture(() => new Response(new Uint8Array(101), { status: 200, headers: { "content-type": "image/gif" } }));
    expect(await fetchGifMedia(url, "t", 100, big.fetchFn)).toEqual({ kind: "unavailable" });
    const foreign = capture(() => new Response(new Uint8Array([1]), { status: 200, headers: { "content-type": "image/gif" } }));
    expect(await fetchGifMedia("https://media.giphy.com/x.gif", "t", 100, foreign.fetchFn)).toEqual({ kind: "unavailable" });
    expect(foreign.calls).toHaveLength(0);
  });

  it("maps 429 on media too", async () => {
    const { fetchFn } = capture(() => json(429, { error: "rate-limited" }, { "retry-after": "2" }));
    expect(await fetchGifMedia(url, "t", 100, fetchFn)).toEqual({ kind: "rate-limited", retryAfterSecs: 2 });
  });
});

describe("debounce", () => {
  function fakeTimers(): Timers & { advance(ms: number): void } {
    let now = 0;
    let seq = 0;
    const pending = new Map<number, { at: number; fn: () => void }>();
    return {
      setTimeout(fn, ms) {
        seq += 1;
        pending.set(seq, { at: now + ms, fn });
        return seq;
      },
      clearTimeout(h) {
        pending.delete(h as number);
      },
      advance(ms) {
        now += ms;
        for (const [id, t] of [...pending]) {
          if (t.at <= now) {
            pending.delete(id);
            t.fn();
          }
        }
      },
    };
  }

  it("fires only the last call, once, after the quiet period", () => {
    const timers = fakeTimers();
    const d = createDebouncer(GIF_SEARCH_DEBOUNCE_MS, timers);
    const fn = vi.fn();
    d.schedule(() => fn("c"));
    timers.advance(200);
    d.schedule(() => fn("ca"));
    timers.advance(200);
    d.schedule(() => fn("cat"));
    timers.advance(GIF_SEARCH_DEBOUNCE_MS - 1);
    expect(fn).not.toHaveBeenCalled();
    timers.advance(1);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith("cat");
  });

  it("flush runs now and cancel drops", () => {
    const timers = fakeTimers();
    const d = createDebouncer(350, timers);
    const fn = vi.fn();
    d.schedule(() => fn(1));
    d.flush();
    expect(fn).toHaveBeenCalledTimes(1);
    timers.advance(1000);
    expect(fn).toHaveBeenCalledTimes(1);
    d.schedule(() => fn(2));
    d.cancel();
    timers.advance(1000);
    expect(fn).toHaveBeenCalledTimes(1);
  });
});
