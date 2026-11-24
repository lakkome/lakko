// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { countAfterRemoval, ownPublisherKeys, publisherReaderRows, shownSubscriberCount } from "./subscriber-count";

describe("what the subscriber count counts (UX 15.1)", () => {
  it("leaves out the publishers' reader rows known to be registered", () => {
    expect(shownSubscriberCount(3, publisherReaderRows("aa", []))).toBe(2);
    expect(shownSubscriberCount(3, publisherReaderRows(null, []))).toBe(3);
    expect(shownSubscriberCount(0, publisherReaderRows("aa", ["bb"]))).toBe(0);
    // Night walk (2026-11-24): owner + one publisher, one real subscriber.
    expect(shownSubscriberCount(3, publisherReaderRows("aa", ["BB"]))).toBe(1);
    // The same key named twice counts once.
    expect(publisherReaderRows("AA", ["aa", "bb", "bb"])).toBe(2);
  });

  it("follows a removal at once and never goes below 0", () => {
    expect(countAfterRemoval(3, 1)).toBe(2);
    expect(countAfterRemoval(1, 4)).toBe(0);
    expect(countAfterRemoval(null, 1)).toBeNull();
  });

  it("names the publishing viewer's own keys only", () => {
    expect(ownPublisherKeys(false, ["AA"])).toEqual([]);
    expect(ownPublisherKeys(true, ["AA", "aa", null, undefined, "", "bb"])).toEqual(["aa", "bb"]);
  });
});
