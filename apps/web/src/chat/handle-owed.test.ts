// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { HandleWriteError } from "../discovery/handle-lookup";
import { DiscoveryNetworkError } from "../discovery/client";
import { mergeOwed, owedOutcomeOf, owedRetryable, type OwedHandle } from "./handle-owed";
import { handleNeedsRenewal, HANDLE_LEASE_MS, HANDLE_RENEW_WHEN_LEFT_MS } from "./group-handle";

const base: OwedHandle = {
  kind: "group",
  id: "g",
  op: "update",
  handle: "alpha",
  oldHandle: null,
  isPublic: true,
  sinceMs: 1,
  reason: "change",
};

describe("owed @name writes", () => {
  it("a claim that never landed stays a claim", () => {
    const merged = mergeOwed({ ...base, op: "claim" }, { ...base, op: "update", isPublic: false, sinceMs: 5 });
    expect(merged.op).toBe("claim");
    expect(merged.isPublic).toBe(false);
    expect(merged.sinceMs).toBe(1);
  });

  it("a second rename starts from the name the server still holds", () => {
    const merged = mergeOwed(
      { ...base, op: "replace", oldHandle: "old", handle: "mid" },
      { ...base, op: "replace", oldHandle: "mid", handle: "new" },
    );
    expect(merged).toMatchObject({ op: "replace", oldHandle: "old", handle: "new" });
  });

  it("a flip after an owed rename keeps the rename owed", () => {
    const merged = mergeOwed(
      { ...base, op: "replace", oldHandle: "old", handle: "mid" },
      { ...base, op: "update", handle: "mid", isPublic: false },
    );
    expect(merged).toMatchObject({ op: "replace", oldHandle: "old", handle: "mid", isPublic: false });
  });

  it("a renewal never replaces what the person asked for", () => {
    const asked = { ...base, isPublic: false };
    expect(mergeOwed(asked, { ...base, reason: "renew" })).toEqual(asked);
  });

  it("a re-key rides with whatever else is owed, oldest key first", () => {
    const rekey = { ...base, reason: "rekey" as const, rekeyFrom: "k1" };
    const asked = { ...base, isPublic: false };
    expect(mergeOwed(asked, rekey)).toEqual({ ...asked, rekeyFrom: "k1" });
    expect(mergeOwed(rekey, asked)).toEqual({ ...asked, rekeyFrom: "k1" });
    expect(mergeOwed({ ...rekey, rekeyFrom: "k0" }, { ...rekey, rekeyFrom: "k1" }).rekeyFrom).toBe("k0");
    // A claim never reached the server: no re-key.
    expect(mergeOwed({ ...base, op: "claim" }, rekey).rekeyFrom).toBeUndefined();
  });

  it("retries what can pass and drops a refusal for good", () => {
    expect(owedRetryable(null)).toBe(true);
    expect(owedRetryable(429)).toBe(true);
    expect(owedRetryable(503)).toBe(true);
    expect(owedRetryable(409)).toBe(false);
    expect(owedOutcomeOf(new HandleWriteError(409, "name-taken"))).toEqual({ dropped: "name-taken" });
    expect(owedOutcomeOf(new HandleWriteError(502, null))).toBe("retry");
    expect(owedOutcomeOf(new DiscoveryNetworkError("/x", new TypeError("offline")))).toBe("retry");
  });
});

describe("the lease", () => {
  const now = 1_000 * HANDLE_LEASE_MS;
  it("renews with fewer than 90 days left", () => {
    expect(handleNeedsRenewal(now - (HANDLE_LEASE_MS - HANDLE_RENEW_WHEN_LEFT_MS) + 1, 0, now)).toBe(false);
    expect(handleNeedsRenewal(now - (HANDLE_LEASE_MS - HANDLE_RENEW_WHEN_LEFT_MS) - 1, 0, now)).toBe(true);
  });
  it("takes the later of the last write and the answered lease", () => {
    expect(handleNeedsRenewal(now - HANDLE_LEASE_MS, now + HANDLE_LEASE_MS, now)).toBe(false);
  });
  it("renews a name it knows nothing about", () => {
    expect(handleNeedsRenewal(0, 0, now)).toBe(true);
  });
});
