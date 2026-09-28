// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0056 security follow-up: a join-by-name device restore lands only
 * when the typed name resolves, at discovery, to the account the grant
 * redeem answered for. Every other outcome refuses before anything is
 * persisted, zeroes the transferred seed and drops the session.
 */
import { describe, expect, it, vi } from "vitest";

import {
  discoveryAccountLookup,
  runNameCheckedDeviceRestore,
  TYPED_NAME_CHECK_UNAVAILABLE,
  TYPED_NAME_OTHER_ACCOUNT,
  TypedNameCheckUnavailable,
  TypedNameOtherAccount,
} from "./device-restore-name-check";
import { AnonTokenQuotaExhaustedError, AnonTokensDisabledError } from "../tokens/anon-token-pool";
import { DiscoveryNetworkError } from "../discovery/client";

const REDEEMED = new Uint8Array(16).fill(0xa1);
const OTHER = new Uint8Array(16).fill(0xb2);

function harness(lookup: (username: string) => Promise<Uint8Array | null>) {
  const phraseSeed = new Uint8Array(64).fill(7);
  const calls: string[] = [];
  const land = vi.fn(async () => {
    calls.push("land");
  });
  const dropSession = vi.fn(() => {
    calls.push("drop");
  });
  const run = (): Promise<void> =>
    runNameCheckedDeviceRestore({
      username: "alice",
      accountIdBytes: REDEEMED,
      phraseSeed,
      sessionToken: "v1.acct.1.tag",
      verifySeed: async () => {
        calls.push("verify");
      },
      attachSession: (token) => {
        calls.push(`attach:${token}`);
      },
      dropSession,
      lookup: async (name) => {
        calls.push(`lookup:${name}`);
        return lookup(name);
      },
      land,
    });
  return { phraseSeed, calls, land, dropSession, run };
}

function zeroed(bytes: Uint8Array): boolean {
  return bytes.every((b) => b === 0);
}

describe("runNameCheckedDeviceRestore", () => {
  it("lands when the typed name resolves to the redeemed account", async () => {
    const h = harness(async () => new Uint8Array(REDEEMED));
    await h.run();
    expect(h.calls).toEqual(["verify", "attach:v1.acct.1.tag", "lookup:alice", "land"]);
    expect(h.land).toHaveBeenCalledTimes(1);
    expect(h.dropSession).not.toHaveBeenCalled();
    expect(zeroed(h.phraseSeed)).toBe(false);
  });

  it("refuses a name that resolves to another account and persists nothing", async () => {
    const h = harness(async () => OTHER);
    const err = await h.run().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(TypedNameOtherAccount);
    expect((err as Error).message).toBe(TYPED_NAME_OTHER_ACCOUNT);
    expect((err as Error).message).toBe(
      "The name you typed belongs to a different account. Nothing was changed on this device.",
    );
    expect(h.land).not.toHaveBeenCalled();
    expect(h.dropSession).toHaveBeenCalledTimes(1);
    expect(zeroed(h.phraseSeed)).toBe(true);
  });

  it("refuses a name discovery does not know", async () => {
    const h = harness(async () => null);
    const err = await h.run().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(TypedNameOtherAccount);
    expect((err as Error).message).toBe(TYPED_NAME_OTHER_ACCOUNT);
    expect(h.land).not.toHaveBeenCalled();
    expect(h.dropSession).toHaveBeenCalledTimes(1);
    expect(zeroed(h.phraseSeed)).toBe(true);
  });

  it("refuses an account id of another length", async () => {
    const h = harness(async () => REDEEMED.subarray(0, 15));
    await expect(h.run()).rejects.toBeInstanceOf(TypedNameOtherAccount);
    expect(h.land).not.toHaveBeenCalled();
  });

  const failures: ReadonlyArray<readonly [string, () => unknown]> = [
    ["network", () => new DiscoveryNetworkError("/discovery/oprf", new TypeError("offline"))],
    ["token quota", () => new AnonTokenQuotaExhaustedError("discovery-lookup", 60)],
    ["tokens disabled", () => new AnonTokensDisabledError("discovery-lookup")],
  ];
  for (const [label, make] of failures) {
    it(`fails closed when the lookup fails (${label})`, async () => {
      const h = harness(async () => {
        throw make();
      });
      const err = await h.run().catch((e: unknown) => e);
      expect(err).toBeInstanceOf(TypedNameCheckUnavailable);
      expect((err as Error).message).toBe(TYPED_NAME_CHECK_UNAVAILABLE);
      expect(h.land).not.toHaveBeenCalled();
      expect(h.dropSession).toHaveBeenCalledTimes(1);
      expect(zeroed(h.phraseSeed)).toBe(true);
    });
  }

  it("never attaches the session or looks up when the seed does not verify", async () => {
    const h = harness(async () => new Uint8Array(REDEEMED));
    const lookup = vi.fn();
    const land = vi.fn();
    const attach = vi.fn();
    await expect(
      runNameCheckedDeviceRestore({
        username: "alice",
        accountIdBytes: REDEEMED,
        phraseSeed: h.phraseSeed,
        sessionToken: "v1.acct.1.tag",
        verifySeed: async () => {
          throw new Error("mismatch");
        },
        attachSession: attach,
        dropSession: vi.fn(),
        lookup,
        land,
      }),
    ).rejects.toThrow("mismatch");
    expect(attach).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
    expect(land).not.toHaveBeenCalled();
  });
});

describe("discoveryAccountLookup", () => {
  it("returns the resolved account, or null for an unknown name", async () => {
    const hit = discoveryAccountLookup({
      lookup: async () => ({ account: REDEEMED }),
    });
    expect(await hit("alice")).toEqual(REDEEMED);
    const miss = discoveryAccountLookup({ lookup: async () => ({ account: null }) });
    expect(await miss("alice")).toBeNull();
  });
});
