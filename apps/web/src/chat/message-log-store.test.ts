// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A wiped account's message log is neither read nor written, and the device
 * key is not touched (ADR-0077): in the desktop app the host releases the key
 * at the wipe, so a late background write used to fail inside the cipher.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { resetAccountStoreGateForTests, sealAccountStoresForWipe } from "../auth/account-store-gate";
import type { DeviceCipher } from "../auth/device-cipher";
import { MESSAGE_LOG_VERSION, saveMessageLog, updateMessageLog } from "./message-log-store";

const A = "QUFBQUFBQUFBQUFBQUFBQQ==";

/** A key that records every use and refuses it, like a released host handle. */
function releasedKey(): { key: DeviceCipher; uses: string[] } {
  const uses: string[] = [];
  const refuse = (name: string) => (): Promise<ArrayBuffer> => {
    uses.push(name);
    return Promise.reject(new Error("host device key: 400 handle"));
  };
  return { key: { encrypt: refuse("encrypt"), decrypt: refuse("decrypt") }, uses };
}

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
  resetAccountStoreGateForTests();
});

describe("message log of a wiped account", () => {
  it("saves nothing and never uses the device key", async () => {
    sealAccountStoresForWipe([A]);
    const { key, uses } = releasedKey();
    await expect(saveMessageLog(A, key, { version: MESSAGE_LOG_VERSION, groups: {} })).resolves.toBeUndefined();
    expect(uses).toEqual([]);
  });

  it("an update neither reads nor writes", async () => {
    sealAccountStoresForWipe([A]);
    const { key, uses } = releasedKey();
    let mutated = false;
    await expect(
      updateMessageLog(A, key, () => {
        mutated = true;
        return { version: MESSAGE_LOG_VERSION, groups: {} };
      }),
    ).resolves.toBeUndefined();
    expect(mutated).toBe(false);
    expect(uses).toEqual([]);
  });
});
