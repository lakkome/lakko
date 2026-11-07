// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { mergeChannelLinks, type ChannelLinkView } from "./channel-link-merge";

function link(over: Partial<ChannelLinkView>): ChannelLinkView {
  return {
    linkIdHex: "aabbccddeeff",
    url: "https://lakko.me/g/x#y",
    label: "",
    expiryMs: 0,
    maxUses: 0xffffffff,
    uses: 0,
    revoked: false,
    mine: true,
    mintedAtMs: 1,
    ...over,
  };
}

describe("mergeChannelLinks", () => {
  it("lists a link made on another device beside this device's", () => {
    const out = mergeChannelLinks([link({ linkIdHex: "01", mintedAtMs: 1 })], [link({ linkIdHex: "02", mintedAtMs: 2, label: "Site" })]);
    expect(out.map((l) => l.linkIdHex)).toEqual(["02", "01"]);
    expect(out[0]?.label).toBe("Site");
  });

  it("keeps one entry per ledger id, whatever the case of its hex", () => {
    const out = mergeChannelLinks([link({ linkIdHex: "AB" })], [link({ linkIdHex: "ab" })]);
    expect(out).toHaveLength(1);
  });

  it("reads a link revoked on another device as revoked", () => {
    const out = mergeChannelLinks([link({})], [link({ revoked: true })]);
    expect(out[0]?.revoked).toBe(true);
  });

  it("never brings a revoked link back", () => {
    const out = mergeChannelLinks([link({ revoked: true })], [link({ revoked: false })]);
    expect(out[0]?.revoked).toBe(true);
  });

  it("takes the label from the register when this device kept none", () => {
    const out = mergeChannelLinks([link({ label: "" })], [link({ label: "Newsletter" })]);
    expect(out[0]?.label).toBe("Newsletter");
  });
});
