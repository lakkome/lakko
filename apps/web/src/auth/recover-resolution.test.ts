// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  resolveRecoverAccount,
  type AccountLookup,
} from "./recover-resolution";

/** Deterministic fake encoder so the test can assert the looked-up bytes were
 *  passed through it. */
const fakeEncode = (bytes: Uint8Array): string =>
  `enc:${Array.from(bytes).join(",")}`;

const lookupOk = (account: Uint8Array, normalised = "alice"): AccountLookup => ({
  account,
  normalisedUsername: normalised,
});

describe("resolveRecoverAccount", () => {
  it("uses the cached account when the typed username is empty (no lookup)", async () => {
    const lookup = vi.fn();
    const resolved = await resolveRecoverAccount({
      cached: { accountIdB64: "CACHED", username: "alice" },
      username: undefined,
      lookup,
      encodeAccountId: fakeEncode,
    });
    expect(resolved).toEqual({ accountIdB64: "CACHED", resolvedUsername: "alice" });
    expect(lookup).not.toHaveBeenCalled();
  });

  it("uses the cached account when the typed username matches it (no lookup)", async () => {
    const lookup = vi.fn();
    const resolved = await resolveRecoverAccount({
      cached: { accountIdB64: "CACHED", username: "alice" },
      username: "  alice  ", // trimmed before compare
      lookup,
      encodeAccountId: fakeEncode,
    });
    expect(resolved).toEqual({ accountIdB64: "CACHED", resolvedUsername: "alice" });
    expect(lookup).not.toHaveBeenCalled();
  });

  it("looks up via discovery on a FRESH device (no cache) given the username", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]);
    const lookup = vi.fn(async () => lookupOk(bytes, "alice"));
    const resolved = await resolveRecoverAccount({
      cached: null,
      username: "alice",
      lookup,
      encodeAccountId: fakeEncode,
    });
    expect(lookup).toHaveBeenCalledWith("alice");
    expect(resolved).toEqual({
      accountIdB64: "enc:1,2,3,4",
      resolvedUsername: "alice",
    });
  });

  it("looks up (not the cache) when the typed username differs from the cached one", async () => {
    const bytes = new Uint8Array([9, 9]);
    const lookup = vi.fn(async () => lookupOk(bytes, "bob"));
    const resolved = await resolveRecoverAccount({
      cached: { accountIdB64: "CACHED", username: "alice" },
      username: "bob",
      lookup,
      encodeAccountId: fakeEncode,
    });
    expect(lookup).toHaveBeenCalledWith("bob");
    expect(resolved).toEqual({ accountIdB64: "enc:9,9", resolvedUsername: "bob" });
  });

  it("throws the bad-phrase-collapsed error when the username is unregistered", async () => {
    const lookup = vi.fn(async (): Promise<AccountLookup> => ({
      account: null,
      normalisedUsername: "ghost",
    }));
    await expect(
      resolveRecoverAccount({
        cached: null,
        username: "ghost",
        lookup,
        encodeAccountId: fakeEncode,
      }),
    ).rejects.toThrow("invalid recovery");
  });

  it("throws the fresh-device prompt when neither a cache nor a username is given", async () => {
    const lookup = vi.fn();
    await expect(
      resolveRecoverAccount({
        cached: null,
        username: "   ", // whitespace-only → empty after trim
        lookup,
        encodeAccountId: fakeEncode,
      }),
    ).rejects.toThrow("no account on this device");
    expect(lookup).not.toHaveBeenCalled();
  });
});
