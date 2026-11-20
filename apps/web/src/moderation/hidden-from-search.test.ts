// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 step 5 - an account moderators hid from search: the refused
 * Everyone publish is recognised, Who can find me holds the state for that
 * account, Everyone cannot be chosen, and a later publish that succeeds
 * leaves the state.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { DiscoveryHttpError } from "../discovery/client";
import { everyoneOffered } from "../settings/who-can-find-me";
import {
  HIDDEN_FROM_SEARCH,
  clearHiddenFromSearch,
  isHiddenFromSearch,
  isHiddenFromSearchError,
  noteEveryonePublished,
  noteHiddenFromSearch,
} from "./hidden-from-search";

afterEach(() => {
  clearHiddenFromSearch();
  vi.unstubAllGlobals();
});

const ENDPOINT = "/discovery/visibility";

describe("hidden from search (ADR-0099 step 5)", () => {
  it("recognises the refused Everyone publish and nothing else", () => {
    expect(isHiddenFromSearchError(new DiscoveryHttpError(403, ENDPOINT, JSON.stringify({ error: HIDDEN_FROM_SEARCH })))).toBe(
      true,
    );
    expect(isHiddenFromSearchError(new DiscoveryHttpError(403, ENDPOINT, JSON.stringify({ error: "account-suspended" })))).toBe(
      false,
    );
    expect(isHiddenFromSearchError(new DiscoveryHttpError(403, ENDPOINT, ""))).toBe(false);
    expect(isHiddenFromSearchError(new DiscoveryHttpError(409, ENDPOINT, JSON.stringify({ error: HIDDEN_FROM_SEARCH })))).toBe(
      false,
    );
    expect(isHiddenFromSearchError(new TypeError("offline"))).toBe(false);
  });

  it("holds the state for the account it was refused for, until a publish succeeds", () => {
    noteHiddenFromSearch("A");
    expect(isHiddenFromSearch("A")).toBe(true);
    expect(isHiddenFromSearch("B")).toBe(false);
    expect(isHiddenFromSearch(null)).toBe(false);
    noteEveryonePublished("B");
    expect(isHiddenFromSearch("A")).toBe(true);
    noteEveryonePublished("A");
    expect(isHiddenFromSearch("A")).toBe(false);
  });

  it("does not offer Everyone while hidden, and offers it again after", () => {
    expect(everyoneOffered(true)).toBe(false);
    expect(everyoneOffered(false)).toBe(true);
  });

  it("carries the server's code on the visibility publish error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: HIDDEN_FROM_SEARCH }), { status: 403 })),
    );
    const { ScopedDiscoveryClient } = await import("../reach/scoped-discovery");
    const client = new ScopedDiscoveryClient(() => "token");
    let caught: unknown = null;
    try {
      await client.putVisibility({ public: true, nameTermLookupKeys: [], publicRecord: new Uint8Array(4) });
    } catch (err) {
      caught = err;
    }
    expect(isHiddenFromSearchError(caught)).toBe(true);
  });
});
