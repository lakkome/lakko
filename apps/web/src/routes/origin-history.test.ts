// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface FakeEntry {
  state: unknown;
  url: string;
}

/** A minimal `window` with a session history, enough for the module. */
function fakeWindow(startUrl: string, startState: unknown = null) {
  const entries: FakeEntry[] = [{ state: startState, url: startUrl }];
  let index = 0;
  const listeners = new Set<() => void>();
  const loc = () => new URL(entries[index]!.url, "https://test.lakko.me");
  const win = {
    history: {
      get state() {
        return entries[index]!.state;
      },
      pushState(state: unknown, _t: string, url: string) {
        entries.splice(index + 1);
        entries.push({ state, url });
        index++;
      },
      replaceState(state: unknown, _t: string, url?: string) {
        entries[index] = { state, url: url ?? entries[index]!.url };
      },
      go(delta: number) {
        index = Math.max(0, Math.min(entries.length - 1, index + delta));
        for (const l of listeners) l();
      },
    },
    location: {
      get pathname() {
        return loc().pathname;
      },
      get search() {
        return loc().search;
      },
      get hash() {
        return loc().hash;
      },
      origin: "https://test.lakko.me",
    },
    addEventListener(_type: string, l: () => void) {
      listeners.add(l);
    },
    removeEventListener(_type: string, l: () => void) {
      listeners.delete(l);
    },
    scrollTo() {},
  };
  return { win, entries: () => entries.map((e) => e.url), current: () => entries[index]! };
}

async function load() {
  vi.resetModules();
  return import("./origin-history");
}

describe("origin-history", () => {
  const saved = (globalThis as { window?: unknown }).window;
  afterEach(() => {
    (globalThis as { window?: unknown }).window = saved;
  });

  let env: ReturnType<typeof fakeWindow>;

  describe("a public page", () => {
    beforeEach(() => {
      env = fakeWindow("/donations");
      (globalThis as { window?: unknown }).window = env.win;
    });

    it("keeps its own address on entry, on navigation and on a reload", async () => {
      const h = await load();
      h.installOriginHistory();
      expect(env.current().url).toBe("/donations");
      expect(h.currentInternalPath()).toBe("/donations");
      h.originHistory.set({ value: "/chat/abc", replace: false, scroll: false });
      expect(env.current().url).toBe("/");
      h.originHistory.set({ value: "/donations", replace: false, scroll: false });
      expect(env.current().url).toBe("/donations");
      const reloaded = await load();
      reloaded.installOriginHistory();
      expect(reloaded.currentInternalPath()).toBe("/donations");
      expect(env.current().url).toBe("/donations");
    });

    it("shows nothing but the origin for the same path with a query or fragment", async () => {
      const h = await load();
      expect(h.addressFor("/donations")).toBe("/donations");
      expect(h.addressFor("/donations?x=1")).toBe("/");
      expect(h.addressFor("/donations#a")).toBe("/");
      expect(h.addressFor("/g/AbCdEfGh#secret")).toBe("/");
    });
  });

  describe("entry from an invite link", () => {
    beforeEach(() => {
      env = fakeWindow("/g/AbCdEfGh#secret16charsxxxxx");
      (globalThis as { window?: unknown }).window = env.win;
    });

    it("clears the address bar and keeps the path for the router", async () => {
      const h = await load();
      h.installOriginHistory();
      expect(env.current().url).toBe("/");
      expect(h.currentInternalPath()).toBe("/g/AbCdEfGh#secret16charsxxxxx");
      expect(h.entrySearch()).toBe("");
    });

    it("never writes a path into the URL on navigation", async () => {
      const h = await load();
      h.installOriginHistory();
      h.originHistory.set({ value: "/chat/abc", replace: false, scroll: false });
      h.originHistory.set({ value: "/contacts", replace: true, scroll: false });
      expect(env.entries()).toEqual(["/", "/"]);
      expect(h.currentInternalPath()).toBe("/contacts");
    });

    it("back returns to the previous screen", async () => {
      const h = await load();
      h.installOriginHistory();
      h.originHistory.set({ value: "/chat/abc", replace: false, scroll: false });
      const seen: unknown[] = [];
      const stop = h.originHistory.listen((v) => seen.push(v));
      h.originHistory.go(-1);
      stop();
      expect(seen).toEqual([{ value: "/g/AbCdEfGh#secret16charsxxxxx", state: undefined }]);
    });

    it("keeps navigation state across back and a reload", async () => {
      const h = await load();
      h.installOriginHistory();
      h.originHistory.set({ value: "/contacts", replace: false, scroll: false, state: { username: "ivan" } });
      expect(h.originHistory.get()).toEqual({ value: "/contacts", state: { username: "ivan" } });
      // A reload re-runs the module against the same session history.
      const again = await load();
      again.installOriginHistory();
      expect(again.currentInternalPath()).toBe("/contacts");
      expect(env.current().url).toBe("/");
    });
  });

  describe("entry from a notification tap", () => {
    beforeEach(() => {
      env = fakeWindow("/?wake=00ff&callAction=accept");
      (globalThis as { window?: unknown }).window = env.win;
    });

    it("exposes the entry query once and a stripped path survives a reload", async () => {
      const h = await load();
      h.installOriginHistory();
      expect(env.current().url).toBe("/");
      expect(h.entrySearch()).toBe("?wake=00ff&callAction=accept");
      h.replaceInternalPath("/");
      const again = await load();
      again.installOriginHistory();
      expect(again.entrySearch()).toBe("");
      expect(again.currentInternalPath()).toBe("/");
    });
  });
});
