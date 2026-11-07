// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { coPublishWaitMs, createWelcomeScanState } from "./co-publish-welcome-scan";

describe("createWelcomeScanState", () => {
  it("keeps the newest row read per mailbox and never moves it back", () => {
    const s = createWelcomeScanState();
    expect(s.cursorOf("aa")).toBe(0);
    s.advance("aa", 22);
    s.advance("aa", 5);
    expect(s.cursorOf("aa")).toBe(22);
    expect(s.cursorOf("bb")).toBe(0);
  });

  it("remembers a Welcome that did not open, within its bound", () => {
    const s = createWelcomeScanState(2);
    s.markFailed("aa:1");
    s.markFailed("aa:2");
    expect(s.hasFailed("aa:1")).toBe(true);
    s.markFailed("aa:3");
    expect(s.hasFailed("aa:1")).toBe(false);
    expect(s.hasFailed("aa:3")).toBe(true);
  });
});

describe("coPublishWaitMs", () => {
  it("starts at 3 s and backs off to 15 s", () => {
    expect(coPublishWaitMs(0)).toBe(3000);
    expect(coPublishWaitMs(1)).toBe(4500);
    expect(coPublishWaitMs(10)).toBe(15_000);
  });
});
