// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { callLinkSlug, callLinkTarget } from "./call-link-slug";

describe("callLinkSlug", () => {
  it("accepts a full link, a bare path and surrounding whitespace", () => {
    expect(callLinkSlug("https://lakko.me/c/abc_DEF-123")).toBe("abc_DEF-123");
    expect(callLinkSlug("/c/abc_DEF-123/")).toBe("abc_DEF-123");
    expect(callLinkSlug("  https://test.lakko.me/c/xyz?x=1#f  ")).toBe("xyz");
  });

  it("refuses anything that is not a call link", () => {
    expect(callLinkSlug("")).toBeNull();
    expect(callLinkSlug("https://lakko.me/i/abc")).toBeNull();
    expect(callLinkSlug("https://lakko.me/c/")).toBeNull();
    expect(callLinkSlug("https://lakko.me/c/a b")).toBeNull();
    expect(callLinkSlug("not a url at all")).toBeNull();
  });

  it("keeps the fragment, which is the link secret", () => {
    expect(callLinkTarget("https://lakko.me/c/abcdefgh#AAAAAAAAAAAAAAAA")).toEqual({
      slug: "abcdefgh",
      fragment: "#AAAAAAAAAAAAAAAA",
    });
    expect(callLinkTarget("/c/abcdefgh")).toEqual({ slug: "abcdefgh", fragment: "" });
  });
});
