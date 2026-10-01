// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { decodeDiscoverPrefs, defaultDiscoverPrefs, encodeDiscoverPrefs } from "./discover-store";

describe("discover prefs codec", () => {
  it("round-trips, including the month's decoy draw and the cache", () => {
    const p = {
      ...defaultDiscoverPrefs(),
      topics: [3, 5],
      language: "en",
      mixRandom: false,
      notInterested: ["aa".repeat(16)],
      hiddenTopics: [7],
      decoys: { month: "2026-09", language: "en", order: [1, 0, 2] },
      lastRefreshMs: 1000,
      lastFetchedKey: "en:3,5",
      buckets: { "3:en": { fetchedAtMs: 1000, items: [{ channel_id: "bb".repeat(16), listing_b64: "x", subscribers: "10+" }] } },
      introDismissed: true,
    };
    expect(decodeDiscoverPrefs(JSON.parse(JSON.stringify(encodeDiscoverPrefs(p))))).toEqual(p);
  });

  it("drops a malformed field alone and defaults the rest", () => {
    const out = decodeDiscoverPrefs({
      version: 1,
      topics: [1, "x", 300, 1],
      language: "EN",
      notInterested: ["nothex"],
      decoys: { month: "sept", language: "en", order: [1] },
      buckets: { "bad key": { fetchedAtMs: 1, items: [] } },
    });
    expect(out.topics).toEqual([1]);
    expect(out.language).toBeNull();
    expect(out.notInterested).toEqual([]);
    expect(out.decoys).toBeNull();
    expect(out.buckets).toEqual({});
    expect(out.mixRandom).toBe(true);
    expect(decodeDiscoverPrefs({ version: 2 })).toEqual(defaultDiscoverPrefs());
  });
});
