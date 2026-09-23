// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

/**
 * What the WEB owns here, and what it does not.
 *
 * The expiry window and the use count are fields of a signed capability, so
 * they are decided once in `lakko_contacts::call_invite` and reached over
 * wasm. Their VALUES are pinned there - the two bounds are `const` assertions
 * that fail the build rather than a test - and asserting them again in
 * TypeScript would only pin the web's guess about a number it no longer
 * holds. That guess is what went wrong: this module shipped its own twelve
 * hours while Android carried twenty-four.
 *
 * So these test the two things the web is responsible for: that it asks the
 * shared source rather than substituting a number of its own, and that when
 * the source is not there it REFUSES instead of guessing - a fallback would
 * be the second, quieter expiry rule all over again.
 */

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    callInviteExpiryMs: (nowMs: bigint) => nowMs + 7n,
    callInviteMaxUses: () => 4_242,
  }),
}));

const values = await import("./call-invite-values");

describe("the values a call link carries", () => {
  it("is one kind of call, with nothing to label", () => {
    // The two the web really does decide: they are what this product sends,
    // not what the format constrains.
    expect(values.CALL_INVITE_MEDIA_VIDEO).toBe(true);
    expect(values.CALL_INVITE_LABEL).toBe("");
  });

  it("asks the shared source rather than carrying its own numbers", () => {
    // The stand-in answers 7 and 4242, which no client would ever choose.
    // Reading them back proves nothing local was substituted on the way.
    expect(values.callInviteExpiryMs(1_000)).toBe(1_007n);
    expect(values.callInviteMaxUses()).toBe(4_242);
  });
});

describe("before the shared source has loaded", () => {
  it("refuses rather than guessing", async () => {
    vi.resetModules();
    vi.doMock("../wasm", () => ({ loadWasm: () => new Promise(() => {}) }));
    const pending = await import("./call-invite-values");
    // A guessed expiry here would be signed into a capability, which is the
    // one place a quiet default becomes a wire fact nobody chose.
    expect(() => pending.callInviteExpiryMs(1_000)).toThrow();
    expect(() => pending.callInviteMaxUses()).toThrow();
  });
});
