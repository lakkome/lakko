// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ASK_AGAIN_AFTER_MS, mayAskAgain, sentRequestState, type SentRequest } from "./requests-sent";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;

function sent(sentAtMs: number): SentRequest {
  return { idB64: "AQ==", name: "Berlin", handle: "", sentAtMs, avatar: null, knockKeyB64: "", knockVersion: 0 };
}

describe("Requests sent (UX 6.2, a day since 2026-11-12)", () => {
  it("under a day: Waiting for an admin, and the device may not ask again", () => {
    expect(ASK_AGAIN_AFTER_MS).toBe(DAY);
    expect(sentRequestState(sent(NOW - DAY + 1), NOW)).toBe("waiting");
    expect(mayAskAgain(sent(NOW - DAY + 1), NOW)).toBe(false);
  });

  it("a day or more: No answer yet, and Ask again is offered", () => {
    expect(sentRequestState(sent(NOW - DAY), NOW)).toBe("no-answer");
    expect(mayAskAgain(sent(NOW - DAY), NOW)).toBe(true);
  });

  it("no request yet: the device may ask", () => {
    expect(mayAskAgain(undefined, NOW)).toBe(true);
  });
});
