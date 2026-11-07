// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { groupLinkOf } from "./group-link";

const ORIGIN = "https://lakko.me";

describe("groupLinkOf", () => {
  it("reads a whole-message /g/ link of this origin", () => {
    expect(groupLinkOf(" https://lakko.me/g/Ab3_x-9Z#s3cr3t_x ", ORIGIN)).toEqual({
      href: "/g/Ab3_x-9Z#s3cr3t_x",
      url: "https://lakko.me/g/Ab3_x-9Z#s3cr3t_x",
    });
  });

  it("ignores text around a link, another origin, a query or no secret", () => {
    expect(groupLinkOf("join https://lakko.me/g/Ab3#x", ORIGIN)).toBeNull();
    expect(groupLinkOf("https://evil.example/g/Ab3#x", ORIGIN)).toBeNull();
    expect(groupLinkOf("https://lakko.me/g/Ab3?a=1#x", ORIGIN)).toBeNull();
    expect(groupLinkOf("https://lakko.me/g/Ab3", ORIGIN)).toBeNull();
    expect(groupLinkOf("https://lakko.me/c/Ab3#x", ORIGIN)).toBeNull();
  });
});
