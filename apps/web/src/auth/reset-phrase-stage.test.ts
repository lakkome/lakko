// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The new phrase of a key reset is in the account's row of the phrase store
 * BEFORE the server step (ADR-0077 decision 3 rule 1, G-47), against the real
 * stores over an in-memory IndexedDB.
 */
import { type DeviceCipher, WebDeviceCipher } from "./device-cipher";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  finishAccountWipe,
  resetAccountStoreGateForTests,
  sealAccountSession,
  sealAccountStoresForWipe,
  takeWipeMark,
} from "./account-store-gate";
import { base64Encode } from "./account-id";
import { getDeviceKey, putDeviceKey } from "./key-store";
import { accountStoreIds } from "./wipe-account";
import { PHRASE_NOT_STORED_KEY, stageResetPhrase } from "./reset-phrase-stage";
import { readDevicePhraseWith } from "../backup/device-phrase";
import { getPhraseRow, putPhrase } from "../backup/phrase-store";
import { KeyedError } from "../l10n/keyed-error";

const A = base64Encode(new Uint8Array(16).fill(0x31));
const OLD = "legal winner thank year wave sausage worth useful legal winner thank yellow";
const NEW = "letter advice cage absurd amount doctor acoustic avoid letter advice cage above";
const SEED = new Uint8Array(64).fill(7);

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

async function aKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]));
}

async function storePhrase(phrase: string, key: DeviceCipher): Promise<void> {
  await putPhrase(A, new TextEncoder().encode(phrase), key);
}

describe("staging a reset phrase", () => {
  it("writes the new phrase before anything else, and a rollback puts the old row back", async () => {
    const key = await aKey();
    await storePhrase(OLD, key);
    const staged = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: key, wipes: takeWipeMark() });
    expect(staged.deviceKey).toBe(key);
    expect(await readDevicePhraseWith(A, key)).toBe(NEW);
    // The server step did not happen.
    expect(await staged.rollback()).toBe("restored");
    expect(await readDevicePhraseWith(A, key)).toBe(OLD);
  });

  it("puts back a row the current key cannot open, byte for byte", async () => {
    const other = await aKey();
    await storePhrase(OLD, other);
    const before = await getPhraseRow(A);
    const key = await aKey();
    const staged = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: key, wipes: takeWipeMark() });
    expect(await staged.rollback()).toBe("restored");
    const after = await getPhraseRow(A);
    expect(after?.ciphertext).toEqual(before?.ciphertext);
    expect(await readDevicePhraseWith(A, other)).toBe(OLD);
  });

  it("is refused, with nothing changed, when the account was wiped since the route began", async () => {
    const key = await aKey();
    await storePhrase(OLD, key);
    const wipes = takeWipeMark(1_000);
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids, 2_000);
    finishAccountWipe(ids);
    const err = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: key, wipes }).catch(
      (e: unknown) => e,
    );
    expect(err).toBeInstanceOf(KeyedError);
    expect((err as KeyedError).key).toBe(PHRASE_NOT_STORED_KEY);
    expect(await readDevicePhraseWith(A, key)).toBe(OLD);
  });

  it("is not refused by a wipe before the route began (a recall's staged wipe), nor by a sign-out", async () => {
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids, 1_000);
    finishAccountWipe(ids);
    sealAccountSession(A);
    const key = await aKey();
    const staged = await stageResetPhrase({
      accountIdB64: A,
      phrase: NEW,
      seed: SEED,
      deviceKey: key,
      wipes: takeWipeMark(2_000),
    });
    expect(await readDevicePhraseWith(A, key)).toBe(NEW);
    staged.close();
  });

  it("with no device key, stores one derived from the new seed first, and a rollback removes both", async () => {
    const staged = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: null, wipes: takeWipeMark() });
    const stored = await getDeviceKey(A);
    expect(stored).not.toBeNull();
    expect(await readDevicePhraseWith(A, staged.deviceKey)).toBe(NEW);
    expect(await staged.rollback()).toBe("restored");
    expect(await getDeviceKey(A)).toBeNull();
    expect(await getPhraseRow(A)).toBeNull();
  });

  it("with no device key in hand, wraps under one another tab already stored", async () => {
    const key = await aKey();
    await putDeviceKey(A, key);
    const staged = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: null, wipes: takeWipeMark() });
    expect(await readDevicePhraseWith(A, key)).toBe(NEW);
    // The key it did not mint stays on a rollback.
    expect(await staged.rollback()).toBe("restored");
    expect(await getDeviceKey(A)).not.toBeNull();
  });

  it("a rollback leaves a row a wipe removed (and the key it minted) alone", async () => {
    const wipes = takeWipeMark(1_000);
    const staged = await stageResetPhrase({ accountIdB64: A, phrase: NEW, seed: SEED, deviceKey: null, wipes });
    // Forget device of the account while the server step was out.
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids, 2_000);
    const { deletePhrase } = await import("../backup/phrase-store");
    await deletePhrase(A);
    finishAccountWipe(ids);
    expect(await staged.rollback()).toBe("not-held");
    expect(await getPhraseRow(A)).toBeNull();
  });
});
