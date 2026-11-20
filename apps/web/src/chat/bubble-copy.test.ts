// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { bubbleCopyText, bubbleOffersEdit, bubbleOffersForward } from "./bubble-copy";

// Base scenario 26 (2026-11-13): Copy took the `/g/` link with its secret
// from a bubble that shows only the invitation card. Copy is "the text I
// see" (message-actions UX 3.2).
describe("what Copy takes from a bubble", () => {
  const link = "https://test.lakko.me/g/AAAAAAAA#fakesecretxyz1";

  it("takes nothing from a bubble drawn as an invitation card", () => {
    expect(bubbleCopyText(link, true, null)).toBeNull();
  });

  it("takes a link drawn without its secret as those words", () => {
    expect(bubbleCopyText(link, false, "https://test.lakko.me/g/AAAAAAAA")).toBe("https://test.lakko.me/g/AAAAAAAA");
  });

  it("takes any other text as shown", () => {
    expect(bubbleCopyText("hello", false, null)).toBe("hello");
    expect(bubbleCopyText("", false, null)).toBeNull();
  });
});

// Follow-up: a message drawn as an invitation card is never edited (Edit
// would put the link with its secret in the composer); Delete stays.
describe("Edit on an invitation card", () => {
  it("is not offered on a bubble drawn as a card", () => {
    expect(bubbleOffersEdit(true)).toBe(false);
    expect(bubbleOffersEdit(false)).toBe(true);
  });
});

describe("Forward on an invitation card (maintainer, 2026-11-13)", () => {
  it("is not offered on a bubble drawn as a card", () => {
    expect(bubbleOffersForward(true)).toBe(false);
    expect(bubbleOffersForward(false)).toBe(true);
  });
});
