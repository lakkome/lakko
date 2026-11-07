// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { addToEndToast, endToastText, type EndToast } from "./end-toast";

const one = (n: string): string => `You are no longer in ${n}.`;
const more = (n: string, c: number): string => `You are no longer in ${n} and ${c} more.`;

describe("the end notice", () => {
  it("names one conversation", () => {
    const toast = addToEndToast(null, ["Book club"]) as EndToast;
    expect(endToastText(toast, one, more)).toBe("You are no longer in Book club.");
  });

  it("removals noticed together make one notice", () => {
    const toast = addToEndToast(null, ["Book club", "News", "Walk"]) as EndToast;
    expect(endToastText(toast, one, more)).toBe("You are no longer in Book club and 2 more.");
  });

  it("a removal while a notice is up replaces it with the combined text", () => {
    const up = addToEndToast(null, ["Book club"]);
    const next = addToEndToast(up, ["News"]) as EndToast;
    expect(endToastText(next, one, more)).toBe("You are no longer in Book club and 1 more.");
  });

  it("nothing added keeps what is up", () => {
    expect(addToEndToast(null, [])).toBeNull();
    const up = addToEndToast(null, ["A"]);
    expect(addToEndToast(up, [])).toBe(up);
  });
});
