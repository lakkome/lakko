// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { showsSafetyNumber } from "./safety-pill";

describe("showsSafetyNumber", () => {
  it("shows the pill for an SFU group call, which is what the old gate hid", () => {
    // `calls/sfu-call-ui.ts` publishes exactly this on `connected`. Under the
    // replaced `props.embedded !== true` gate this rendered on no live call,
    // because `Chat.tsx` always mounts the call embedded.
    expect(showsSafetyNumber({ active: true, shortFingerprint: "3F7A B2C1 9E0D" })).toBe(true);
  });

  it("hides it for the 1x1 path, which has no SFrame and no number", () => {
    // `routes/Chat.tsx` publishes exactly this. The old comment's worry — a
    // "secured" chip over a call with no safety number — is closed here, by the
    // data rather than by a layout flag.
    expect(showsSafetyNumber({ active: false, shortFingerprint: null })).toBe(false);
  });

  it("hides it while keying is armed but the number is not derived yet", () => {
    // An empty chip would claim a verification the user cannot perform.
    expect(showsSafetyNumber({ active: true, shortFingerprint: null })).toBe(false);
  });

  it("hides it when a number is present but E2EE is not active", () => {
    // Should not occur; if it does, the honest answer is not to claim protection.
    expect(showsSafetyNumber({ active: false, shortFingerprint: "3F7A B2C1 9E0D" })).toBe(false);
  });

  it("does not consult anything about the surface asking", () => {
    // The regression guard. The bug was a layout flag answering a cryptographic
    // question; this rule takes no such input, so it cannot regress that way
    // without changing the signature — which is a reviewable edit.
    expect(showsSafetyNumber.length).toBe(1);
  });
});
