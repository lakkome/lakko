// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 - the backup carries the account's own predecessors,
 * and a device restored from the newest phrase binds under the OLDEST key,
 * the one every conversation names. Through the real core: payload build,
 * parse, the key-rotation store, the binding verdict and the minted binding.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import {
  conversationSeed,
  dropUnreadableKeyRotationState,
  getKeyRotationState,
  putKeyRotationState,
  predecessorSeeds,
  EMPTY_KEY_ROTATION_STATE,
} from "../auth/key-rotation-store";
import { readPredecessorAdoptedAtMs } from "../contacts/address-resolver";
import { bindingVerdict } from "../devices/own-conversation-key";
import {
  accountKeyHexOfSeed,
  applyOwnPredecessors,
  boundedOwnPredecessors,
  MAX_CARRIED_PREDECESSORS,
  type CarriedOwnPredecessor,
} from "../devices/state-bundle";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { flattenOwnPredecessors, splitOwnPredecessors } from "./client";

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

const ACCOUNT = "QkJCQkJCQkJCQkJCQkJCQg==";
const seed = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);
const own = (fill: number, adoptedAtMs: number): CarriedOwnPredecessor => ({
  seedB64: base64Encode(seed(fill)),
  adoptedAtMs,
});

async function deviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
}

/** The predecessors as a backup written with `carried` hands them back. */
function throughBackup(carried: readonly CarriedOwnPredecessor[]): CarriedOwnPredecessor[] {
  const { seeds, adoptedAtMs } = flattenOwnPredecessors(carried);
  const payload = wasm.buildBackupPayload(
    new Uint8Array(16).fill(7),
    1n,
    new Uint8Array(32).fill(9),
    [],
    new Uint8Array(0),
    undefined,
    carried.length === 0 ? undefined : seeds,
    carried.length === 0 ? undefined : adoptedAtMs,
  );
  const parsed = wasm.parseBackupPayload(payload);
  try {
    return splitOwnPredecessors(parsed.ownPredecessorSeeds, parsed.ownPredecessorAdoptedAtMs);
  } finally {
    parsed.free();
  }
}

describe("the payload", () => {
  it("carries the seeds oldest first, each with its adoption time", () => {
    const carried = [own(1, 1_700_000_000_000), own(2, 1_700_000_100_000)];
    expect(throughBackup(carried)).toEqual(carried);
  });

  it("a backup of an account that never rotated carries none", () => {
    expect(throughBackup([])).toEqual([]);
  });

  it("over the bound keeps the oldest, the key the conversations name", () => {
    const chain = Array.from({ length: MAX_CARRIED_PREDECESSORS + 3 }, (_, i) => own(i + 1, i + 1));
    const kept = throughBackup(boundedOwnPredecessors(chain));
    expect(kept).toHaveLength(MAX_CARRIED_PREDECESSORS);
    expect(kept[0]).toEqual(chain[0]);
    expect(kept[kept.length - 1]).toEqual(chain[chain.length - 1]);
  });
});

describe("a device restored from the newest phrase", () => {
  it("re-mints its binding under the oldest own predecessor", async () => {
    const key = await deviceKey();
    const oldest = wasm.Identity.fromSeed(seed(1));
    const middle = wasm.Identity.fromSeed(seed(2));
    const current = wasm.Identity.fromSeed(seed(3));

    const restored = throughBackup([own(1, 1_700_000_000_000), own(2, 1_700_000_100_000)]);
    await applyOwnPredecessors({ ownPredecessors: restored }, ACCOUNT, key, (s) =>
      accountKeyHexOfSeed(wasm, s),
    );

    // Stored where a kept device holds them, in the same order.
    const state = await getKeyRotationState(ACCOUNT, key);
    expect(predecessorSeeds(state).map((s) => base64Encode(s))).toEqual(restored.map((p) => p.seedB64));
    const conversation = conversationSeed(state);
    expect(conversation).not.toBeNull();

    // The binding the landing would have kept under the current key is not.
    const predecessorKeysB64 = predecessorSeeds(state).map((s) =>
      base64Encode(wasm.Identity.fromSeed(s).signaturePublicKey),
    );
    expect(
      bindingVerdict({
        boundKeyB64: base64Encode(current.signaturePublicKey),
        currentKeyB64: base64Encode(current.signaturePublicKey),
        predecessorKeysB64,
        holdsConversations: false,
      }),
    ).toBe("remint");
    expect(
      bindingVerdict({
        boundKeyB64: null,
        currentKeyB64: base64Encode(current.signaturePublicKey),
        predecessorKeysB64,
        holdsConversations: false,
      }),
    ).toBe("mint");

    // And what is minted names the key the conversations are under.
    const bound = wasm.Identity.fromSeed(conversation as Uint8Array);
    const device = wasm.Identity.fromSeedBound(
      new Uint8Array(32).fill(0x44),
      bound.ed25519Seed,
      BigInt(1_700_000_000),
    );
    expect(base64Encode(device.accountPublicKey as Uint8Array)).toBe(
      base64Encode(oldest.signaturePublicKey),
    );
    expect(base64Encode(device.accountPublicKey as Uint8Array)).not.toBe(
      base64Encode(middle.signaturePublicKey),
    );
  });

  it("each mailbox window counts from the adoption time the backup carried", async () => {
    const key = await deviceKey();
    await applyOwnPredecessors(
      { ownPredecessors: [own(1, 1_700_000_000_000)] },
      ACCOUNT,
      key,
      (s) => accountKeyHexOfSeed(wasm, s),
    );
    expect(readPredecessorAdoptedAtMs(ACCOUNT, accountKeyHexOfSeed(wasm, seed(1)))).toBe(1_700_000_000_000);
  });

  it("a row sealed under a device key that is gone gives way to the restored one", async () => {
    await putKeyRotationState(ACCOUNT, EMPTY_KEY_ROTATION_STATE, await deviceKey());
    const key = await deviceKey();
    await expect(getKeyRotationState(ACCOUNT, key)).rejects.toThrow();
    await dropUnreadableKeyRotationState(ACCOUNT, key);
    await applyOwnPredecessors({ ownPredecessors: [own(1, 5)] }, ACCOUNT, key, (s) =>
      accountKeyHexOfSeed(wasm, s),
    );
    expect(conversationSeed(await getKeyRotationState(ACCOUNT, key))).toEqual(seed(1));
  });

  it("a row that opens is left alone", async () => {
    const key = await deviceKey();
    await applyOwnPredecessors({ ownPredecessors: [own(1, 5)] }, ACCOUNT, key, (s) =>
      accountKeyHexOfSeed(wasm, s),
    );
    await dropUnreadableKeyRotationState(ACCOUNT, key);
    expect(conversationSeed(await getKeyRotationState(ACCOUNT, key))).toEqual(seed(1));
  });
});
