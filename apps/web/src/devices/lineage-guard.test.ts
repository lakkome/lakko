// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 "Joining after a rotation", the DeviceGroup lineage guard
 * (contract G-56 section 2.4), and the own-link record it reads
 * (`auth/own-conversation-key-store.ts`): with a recorded link naming `C`,
 * the DeviceGroup id derives from the held seed of `C`, and from nothing
 * when no held seed derives it; with no record the fallback is unchanged.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { putAccountIdentitySeed } from "../auth/account-identity-seed-store";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import {
  EMPTY_KEY_ROTATION_STATE,
  getKeyRotationState,
  putKeyRotationState,
} from "../auth/key-rotation-store";
import { readOwnLinkRecord, writeOwnLinkRecord } from "../auth/own-conversation-key-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { OWN_LINK_ABSENT } from "./own-conversation-key";
import { deviceGroupLineageSeed } from "./lifecycle";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
});
afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

const ACCOUNT = "Q0NDQ0NDQ0NDQ0NDQ0NDQw==";
const seed = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);
const keyOf = (s: Uint8Array): Uint8Array => {
  const id = wasm.Identity.fromSeed(s);
  try {
    return id.signaturePublicKey;
  } finally {
    id.free();
  }
};

async function deviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
}

describe("the own-link record", () => {
  it("is null until read, then holds the key or 'absent'; unknown records nothing", async () => {
    const dk = await deviceKey();
    expect(await readOwnLinkRecord(ACCOUNT, dk)).toBeNull();
    await writeOwnLinkRecord(ACCOUNT, dk, { kind: "unknown" });
    expect(await readOwnLinkRecord(ACCOUNT, dk)).toBeNull();
    const c = keyOf(seed(1));
    await writeOwnLinkRecord(ACCOUNT, dk, { kind: "key", key: c });
    const read = await readOwnLinkRecord(ACCOUNT, dk);
    expect(read?.kind).toBe("key");
    expect(read?.kind === "key" ? Array.from(read.key) : []).toEqual(Array.from(c));
    await writeOwnLinkRecord(ACCOUNT, dk, OWN_LINK_ABSENT);
    expect(await readOwnLinkRecord(ACCOUNT, dk)).toEqual(OWN_LINK_ABSENT);
  });

  it("keeps the predecessor seeds it sits beside", async () => {
    const dk = await deviceKey();
    await putKeyRotationState(
      ACCOUNT,
      { ...EMPTY_KEY_ROTATION_STATE, predecessorSeedsB64: [base64Encode(seed(1))] },
      dk,
    );
    await writeOwnLinkRecord(ACCOUNT, dk, { kind: "key", key: keyOf(seed(1)) });
    expect((await getKeyRotationState(ACCOUNT, dk)).predecessorSeedsB64).toEqual([base64Encode(seed(1))]);
  });
});

describe("deviceGroupLineageSeed (the lineage guard)", () => {
  it("answers null with a recorded link naming C and no held seed of C", async () => {
    const dk = await deviceKey();
    await putAccountIdentitySeed(ACCOUNT, seed(9), dk);
    await writeOwnLinkRecord(ACCOUNT, dk, { kind: "key", key: keyOf(seed(1)) });
    expect(await deviceGroupLineageSeed(ACCOUNT, dk)).toBeNull();
    // An explicit current seed changes nothing: it is not C either.
    expect(await deviceGroupLineageSeed(ACCOUNT, dk, seed(9))).toBeNull();
  });

  it("answers the held seed of C once it arrived", async () => {
    const dk = await deviceKey();
    await putAccountIdentitySeed(ACCOUNT, seed(9), dk);
    await writeOwnLinkRecord(ACCOUNT, dk, { kind: "key", key: keyOf(seed(1)) });
    await putKeyRotationState(
      ACCOUNT,
      {
        ...(await getKeyRotationState(ACCOUNT, dk)),
        predecessorSeedsB64: [base64Encode(seed(1)), base64Encode(seed(2))],
      },
      dk,
    );
    expect(Array.from((await deviceGroupLineageSeed(ACCOUNT, dk)) ?? [])).toEqual(Array.from(seed(1)));
  });

  it("falls back as before with no record: the oldest predecessor, else the account seed", async () => {
    const dk = await deviceKey();
    await putAccountIdentitySeed(ACCOUNT, seed(9), dk);
    expect(Array.from((await deviceGroupLineageSeed(ACCOUNT, dk)) ?? [])).toEqual(Array.from(seed(9)));
    await putKeyRotationState(
      ACCOUNT,
      { ...EMPTY_KEY_ROTATION_STATE, predecessorSeedsB64: [base64Encode(seed(1))] },
      dk,
    );
    expect(Array.from((await deviceGroupLineageSeed(ACCOUNT, dk)) ?? [])).toEqual(Array.from(seed(1)));
  });

  it("a recorded 'no link' changes nothing", async () => {
    const dk = await deviceKey();
    await putAccountIdentitySeed(ACCOUNT, seed(9), dk);
    await writeOwnLinkRecord(ACCOUNT, dk, OWN_LINK_ABSENT);
    expect(Array.from((await deviceGroupLineageSeed(ACCOUNT, dk)) ?? [])).toEqual(Array.from(seed(9)));
  });
});
