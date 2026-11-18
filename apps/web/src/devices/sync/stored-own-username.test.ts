// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stored own-username register, read with no DeviceGroup running: what a
 * restore wrote is what the rejoin knock and the landing's username step
 * (`docs/ux/identity-recovery.md` section 8b) find.
 */
import { describe, expect, it } from "vitest";

import type { DeviceCipher } from "../../auth/device-cipher";
import { storedOwnUsername, type StoredOwnUsernameDeps } from "./stored-own-username";

const KEY = {} as DeviceCipher;

function deps(bytes: Uint8Array | null, register: string | undefined, opens = true) {
  const freed: number[] = [];
  const d: StoredOwnUsernameDeps = {
    load: async () => bytes,
    open: async () =>
      opens
        ? {
            ownUsername: () => register,
            free: () => {
              freed.push(1);
            },
          }
        : null,
  };
  return { d, freed };
}

describe("the stored own-username register", () => {
  it("the name the stored state holds, trimmed, and the fold is released", async () => {
    const { d, freed } = deps(new Uint8Array([1]), " anna ");
    expect(await storedOwnUsername("acct", KEY, d)).toBe("anna");
    expect(freed).toHaveLength(1);
  });

  it("nothing stored, a register naming none, or a fold that does not decode: empty", async () => {
    expect(await storedOwnUsername("acct", KEY, deps(null, "anna").d)).toBe("");
    expect(await storedOwnUsername("acct", KEY, deps(new Uint8Array([1]), undefined).d)).toBe("");
    expect(await storedOwnUsername("acct", KEY, deps(new Uint8Array([1]), "anna", false).d)).toBe("");
  });

  it("a store that cannot be read throws, for the caller to log", async () => {
    const d: StoredOwnUsernameDeps = {
      load: async () => {
        throw new Error("idb");
      },
      open: async () => null,
    };
    await expect(storedOwnUsername("acct", KEY, d)).rejects.toThrow("idb");
  });
});
