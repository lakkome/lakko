// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The call-link page's two decisions (`link-join-gate.ts`): no guest account
 * for a call that cannot start, and when a guest is asked about the account
 * (`docs/ux/call-links.md` section 9.1).
 */
import { describe, expect, it } from "vitest";

import { linkJoinRefusal, showsGuestQuestion } from "./link-join-gate";

describe("linkJoinRefusal", () => {
  it("lets a join go on when media can be encrypted and no call holds the device", () => {
    expect(linkJoinRefusal({ canEncryptMedia: true, busy: false })).toBeNull();
  });

  it("refuses, before any account is made, a browser that cannot encrypt call media", () => {
    expect(linkJoinRefusal({ canEncryptMedia: false, busy: false })).toBe("no-media-encryption");
  });

  it("names the encryption refusal first: ending the other call would not help", () => {
    expect(linkJoinRefusal({ canEncryptMedia: false, busy: true })).toBe("no-media-encryption");
  });

  it("refuses while another call holds the device", () => {
    expect(linkJoinRefusal({ canEncryptMedia: true, busy: true })).toBe("busy");
  });
});

describe("showsGuestQuestion", () => {
  it("asks a guest who is not in a call: before joining again, and after a join that failed", () => {
    expect(showsGuestQuestion({ isGuest: true, phase: "idle" })).toBe(true);
    expect(showsGuestQuestion({ isGuest: true, phase: "error" })).toBe(true);
  });

  it("does not ask while the call is being started, waited for, or live", () => {
    for (const phase of ["starting", "waiting", "live"]) {
      expect(showsGuestQuestion({ isGuest: true, phase })).toBe(false);
    }
  });

  it("leaves the ended and dead states to ask it themselves, once", () => {
    expect(showsGuestQuestion({ isGuest: true, phase: "ended" })).toBe(false);
    expect(showsGuestQuestion({ isGuest: true, phase: "dead" })).toBe(false);
  });

  it("never asks a visitor who is not a guest: no account was made for them", () => {
    for (const phase of ["idle", "error", "starting", "ended"]) {
      expect(showsGuestQuestion({ isGuest: false, phase })).toBe(false);
    }
  });
});
