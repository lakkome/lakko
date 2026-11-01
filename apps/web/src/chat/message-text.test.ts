// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { linkOpenOf, messageTextPieces } from "./message-text";

const ORIGIN = "https://lakko.me";
const INVITE = `${ORIGIN}/i/abcdefgh#0123456789abcdef`;

describe("messageTextPieces", () => {
  it("is one plain piece until the rule is loaded", () => {
    expect(messageTextPieces("see https://a.example", null)).toEqual([
      { text: "see https://a.example", isLink: false },
    ]);
    expect(messageTextPieces("", null)).toEqual([]);
  });

  it("copies each piece out of wasm memory and frees it", () => {
    const free = vi.fn();
    const pieces = messageTextPieces("x", () => [
      { text: "see ", isLink: false, free },
      { text: "https://a.example", isLink: true, free },
    ]);
    expect(pieces).toEqual([
      { text: "see ", isLink: false },
      { text: "https://a.example", isLink: true },
    ]);
    expect(free).toHaveBeenCalledTimes(2);
  });
});

describe("linkOpenOf", () => {
  it("opens a link of this deployment in place, on web and on the desktop", () => {
    const inApp = { kind: "in-app", path: "/i/abcdefgh#0123456789abcdef" };
    expect(linkOpenOf(INVITE, ORIGIN, false)).toEqual(inApp);
    expect(linkOpenOf(INVITE, ORIGIN, true)).toEqual(inApp);
  });

  it("never opens a link of another host in place", () => {
    const foreign = "https://evil.example/i/abcdefgh#0123456789abcdef";
    expect(linkOpenOf(foreign, ORIGIN, false)).toEqual({ kind: "new-tab" });
    expect(linkOpenOf(foreign, ORIGIN, true)).toEqual({ kind: "system-browser" });
  });

  it("sends a page of our own site that is no invite out, like any other link", () => {
    expect(linkOpenOf(`${ORIGIN}/about`, ORIGIN, false)).toEqual({ kind: "new-tab" });
    expect(linkOpenOf(`${ORIGIN}/i/abcdefgh`, ORIGIN, true)).toEqual({ kind: "system-browser" });
  });
});
