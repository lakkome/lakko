// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074: the pairing state bundle v2 and a device paired AFTER a rotation
 * of the account keys. The codec and its bounds, the receive rule (what is
 * stored, the mailbox window, a contact's proven predecessors), and - through
 * the real core - that the paired device binds its conversation identity
 * under the same account key as the sending device.
 */
import { type DeviceCipher, WebDeviceCipher } from "../auth/device-cipher";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, { Identity } from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import {
  EMPTY_KEY_ROTATION_STATE,
  conversationSeed,
  getKeyRotationState,
  predecessorSeeds,
  putKeyRotationState,
  withPredecessorSeed,
} from "../auth/key-rotation-store";
import {
  PREDECESSOR_MAILBOX_READ_MS,
  readPredecessorAdoptedAtMs,
  readablePredecessorKeys,
  stampPredecessorAdoption,
} from "../contacts/address-resolver";
import {
  accountKeysOf,
  mergeCarriedProvenPredecessors,
  readPin,
  setPin,
} from "../contacts/local-store";
import { buildPredecessorTable, conversationKeyOf, samePerson } from "../contacts/predecessor";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  MAX_CARRIED_PREDECESSORS,
  applyOwnPredecessors,
  applyStateBundle,
  buildStateBundle,
  collectOwnPredecessors,
  decodeStateBundle,
  encodeStateBundle,
  mergeCarriedPredecessorSeeds,
  type StateBundle,
} from "./state-bundle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const SENDER = "c2VuZGVyLWRldmljZS0wMDAwMA==";
const JOINER = "am9pbmVyLWRldmljZS0wMDAwMA==";
const seed = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);
const key32 = (fill: number): string => base64Encode(new Uint8Array(32).fill(fill));
const hexOf = (bytes: Uint8Array): string =>
  [...bytes].map((x) => x.toString(16).padStart(2, "0")).join("");

function keyHexOfSeed(s: Uint8Array): string {
  const identity = Identity.fromSeed(s);
  try {
    return hexOf(identity.signaturePublicKey);
  } finally {
    identity.free();
  }
}

async function deviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]));
}

function encodeRaw(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

const base: StateBundle = {
  v: 4,
  contacts: [],
  pins: [
    {
      accountIdB64: "ABEiM0RVZneImaq7zN3u/w==",
      pinnedSignaturePubkeyB64: key32(9),
      rotationMarker: 5,
      provenPredecessorsB64: [key32(7), key32(8)],
    },
  ],
  knownGroupIdsB64: [],
  ownPredecessors: [
    { seedB64: base64Encode(seed(1)), adoptedAtMs: 1_700_000_000_000 },
    { seedB64: base64Encode(seed(2)), adoptedAtMs: 1_700_000_100_000 },
  ],
  groupNames: [],
  profiles: [],
  pendingContacts: [],
  channelSeeds: [],
};

let local: MemoryStorage;

beforeEach(() => {
  local = new MemoryStorage();
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = local;
  g["indexedDB"] = new FakeIndexedDb();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

describe("state bundle v2 codec (ADR-0074)", () => {
  it("round-trips the own predecessors and each pin's proven predecessors", () => {
    expect(decodeStateBundle(encodeStateBundle(base))).toEqual(base);
  });

  it("refuses v1 and a bundle that lacks either v2 field", () => {
    expect(decodeStateBundle(encodeRaw({ v: 1, contacts: [], pins: [], knownGroupIdsB64: [] }))).toBeNull();
    const { ownPredecessors: _own, ...withoutOwn } = base;
    expect(decodeStateBundle(encodeRaw(withoutOwn))).toBeNull();
    const pinWithout = { ...base, pins: [{ accountIdB64: "a", pinnedSignaturePubkeyB64: "b", rotationMarker: 0 }] };
    expect(decodeStateBundle(encodeRaw(pinWithout))).toBeNull();
  });

  it("accepts 8 own predecessors and refuses 9", () => {
    const entry = (i: number) => ({ seedB64: base64Encode(seed(i + 1)), adoptedAtMs: 1000 + i });
    const eight = { ...base, ownPredecessors: Array.from({ length: MAX_CARRIED_PREDECESSORS }, (_, i) => entry(i)) };
    expect(decodeStateBundle(encodeRaw(eight))?.ownPredecessors).toHaveLength(8);
    const nine = { ...base, ownPredecessors: Array.from({ length: 9 }, (_, i) => entry(i)) };
    expect(decodeStateBundle(encodeRaw(nine))).toBeNull();
  });

  it("refuses a wrong seed length and a non-integer time, whole bundle", () => {
    const short = { ...base, ownPredecessors: [{ seedB64: base64Encode(new Uint8Array(32)), adoptedAtMs: 1 }] };
    expect(decodeStateBundle(encodeRaw(short))).toBeNull();
    const notB64 = { ...base, ownPredecessors: [{ seedB64: "***", adoptedAtMs: 1 }] };
    expect(decodeStateBundle(encodeRaw(notB64))).toBeNull();
    for (const adoptedAtMs of [1.5, "7", null]) {
      const bad = { ...base, ownPredecessors: [{ seedB64: base64Encode(seed(1)), adoptedAtMs }] };
      expect(decodeStateBundle(encodeRaw(bad))).toBeNull();
    }
  });

  it("accepts 8 proven predecessors of a contact and refuses 9 or a wrong key length", () => {
    const pin = (keys: string[]) => ({ ...base, pins: [{ ...base.pins[0], provenPredecessorsB64: keys }] });
    const eight = Array.from({ length: 8 }, (_, i) => key32(i + 1));
    expect(decodeStateBundle(encodeRaw(pin(eight)))?.pins[0]?.provenPredecessorsB64).toEqual(eight);
    expect(decodeStateBundle(encodeRaw(pin([...eight, key32(20)])))).toBeNull();
    expect(decodeStateBundle(encodeRaw(pin([base64Encode(new Uint8Array(31))])))).toBeNull();
    expect(decodeStateBundle(encodeRaw(pin([7 as unknown as string])))).toBeNull();
  });

  it("an account that never rotated carries an empty array", () => {
    const built = buildStateBundle([], SENDER);
    expect(built.v).toBe(4);
    expect(built.ownPredecessors).toEqual([]);
    expect(decodeStateBundle(encodeStateBundle(built))).toEqual(built);
  });
});

describe("the sending device fills the bundle from its own stores", () => {
  it("own predecessors oldest first, each with its adoption time; a missing time is stamped now", async () => {
    const dk = await deviceKey();
    let state = withPredecessorSeed(EMPTY_KEY_ROTATION_STATE, seed(1));
    state = withPredecessorSeed(state, seed(2));
    await putKeyRotationState(SENDER, state, dk);
    stampPredecessorAdoption(SENDER, keyHexOfSeed(seed(1)), 1_700_000_000_000);
    const own = await collectOwnPredecessors(SENDER, dk, keyHexOfSeed, 1_700_000_500_000);
    expect(own).toEqual([
      { seedB64: base64Encode(seed(1)), adoptedAtMs: 1_700_000_000_000 },
      { seedB64: base64Encode(seed(2)), adoptedAtMs: 1_700_000_500_000 },
    ]);
    expect(readPredecessorAdoptedAtMs(SENDER, keyHexOfSeed(seed(2)))).toBe(1_700_000_500_000);
  });

  it("a pin carries the contact's proven predecessors", () => {
    const accountIdB64 = "ABEiM0RVZneImaq7zN3u/w==";
    local.setItem(
      `lakko.contacts.v3.${SENDER}`,
      JSON.stringify({
        v: 3,
        accepted: [
          {
            signaturePubkeyB64: key32(7),
            x25519PubkeyB64: key32(1),
            displayName: "Bob",
            acceptedAtMs: 1,
            accountIdHex: "00112233445566778899aabbccddeeff",
          },
        ],
        pins: {
          [accountIdB64]: {
            pinnedSignaturePubkeyB64: key32(9),
            rotationMarker: 5,
            provenPredecessorsB64: [key32(7), key32(8)],
          },
        },
      }),
    );
    const built = buildStateBundle([], SENDER);
    expect(built.pins).toEqual([
      {
        accountIdB64,
        pinnedSignaturePubkeyB64: key32(9),
        rotationMarker: 5,
        provenPredecessorsB64: [key32(7), key32(8)],
      },
    ]);
  });
});

describe("the receiving device stores what a kept device that adopted holds", () => {
  it("stores the chain oldest first under the device key, and reports what was new", async () => {
    const dk = await deviceKey();
    expect(await applyOwnPredecessors(base, JOINER, dk, keyHexOfSeed)).toBe(2);
    const state = await getKeyRotationState(JOINER, dk);
    expect(state.predecessorSeedsB64).toEqual([base64Encode(seed(1)), base64Encode(seed(2))]);
    expect(Array.from(conversationSeed(state) as Uint8Array)).toEqual(Array.from(seed(1)));
    expect(predecessorSeeds(state)).toHaveLength(2);
    // Applied again: nothing new, nothing reordered.
    expect(await applyOwnPredecessors(base, JOINER, dk, keyHexOfSeed)).toBe(0);
    expect((await getKeyRotationState(JOINER, dk)).predecessorSeedsB64).toEqual(state.predecessorSeedsB64);
  });

  it("no seed reaches localStorage", async () => {
    const dk = await deviceKey();
    await applyOwnPredecessors(base, JOINER, dk, keyHexOfSeed);
    for (const [, value] of local.entries()) {
      expect(value).not.toContain(base64Encode(seed(1)));
      expect(value).not.toContain(base64Encode(seed(2)));
    }
  });

  it("each mailbox window counts from the SENDER's adoption time", async () => {
    const dk = await deviceKey();
    await applyOwnPredecessors(base, JOINER, dk, keyHexOfSeed);
    const first = keyHexOfSeed(seed(1));
    const second = keyHexOfSeed(seed(2));
    expect(readPredecessorAdoptedAtMs(JOINER, first)).toBe(1_700_000_000_000);
    expect(readPredecessorAdoptedAtMs(JOINER, second)).toBe(1_700_000_100_000);
    const adopted = (k: string): number | null => readPredecessorAdoptedAtMs(JOINER, k);
    // Inside both windows; then past the first only; then past both.
    const t0 = 1_700_000_000_000 + PREDECESSOR_MAILBOX_READ_MS;
    expect(readablePredecessorKeys([first, second], adopted, t0 - 1)).toEqual([first, second]);
    expect(readablePredecessorKeys([first, second], adopted, t0 + 1)).toEqual([second]);
    expect(readablePredecessorKeys([first, second], adopted, t0 + 100_001)).toEqual([]);
  });

  it("a time this device already holds for a predecessor is kept", async () => {
    const dk = await deviceKey();
    stampPredecessorAdoption(JOINER, keyHexOfSeed(seed(1)), 1_600_000_000_000);
    await applyOwnPredecessors(base, JOINER, dk, keyHexOfSeed);
    expect(readPredecessorAdoptedAtMs(JOINER, keyHexOfSeed(seed(1)))).toBe(1_600_000_000_000);
  });

  it("merging: the carried chain leads, a held seed it does not name is kept, a superset is unchanged", () => {
    expect(mergeCarriedPredecessorSeeds([], ["a", "b"])).toEqual(["a", "b"]);
    expect(mergeCarriedPredecessorSeeds(["b"], ["a", "b"])).toEqual(["a", "b"]);
    expect(mergeCarriedPredecessorSeeds(["x"], ["a", "b"])).toEqual(["a", "b", "x"]);
    expect(mergeCarriedPredecessorSeeds(["a", "b", "c"], ["b", "c"])).toEqual(["a", "b", "c"]);
    expect(mergeCarriedPredecessorSeeds(["a"], [])).toEqual(["a"]);
  });

  it("a bundle with no own predecessors writes nothing", async () => {
    const dk = await deviceKey();
    expect(await applyOwnPredecessors({ ...base, ownPredecessors: [] }, JOINER, dk, keyHexOfSeed)).toBe(0);
    expect(await getKeyRotationState(JOINER, dk)).toEqual(EMPTY_KEY_ROTATION_STATE);
  });

  it("records each contact's proven predecessors, so an old-key leaf names the same person", () => {
    applyStateBundle(base, JOINER);
    const accountIdB64 = base.pins[0]?.accountIdB64 ?? "";
    expect(readPin(accountIdB64, JOINER)?.provenPredecessorsB64).toEqual([key32(7), key32(8)]);
    const keys = accountKeysOf(accountIdB64, JOINER);
    expect(keys).toBeDefined();
    if (keys === undefined) return;
    const table = buildPredecessorTable([keys]);
    expect(samePerson(key32(7), key32(9), table)).toBe(true);
    expect(samePerson(key32(8), key32(9), table)).toBe(true);
  });

  it("a pin this device holds under ANOTHER key keeps its own chain", () => {
    const accountIdB64 = base.pins[0]?.accountIdB64 ?? "";
    // A higher marker than the bundle's, under another key: the carried pin
    // is refused, and its predecessors are not attached to this one.
    expect(setPin(accountIdB64, key32(30), 99, JOINER)).toBe("set");
    applyStateBundle(base, JOINER);
    const pin = readPin(accountIdB64, JOINER);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(key32(30));
    expect(pin?.provenPredecessorsB64).toBeUndefined();
  });

  it("merging a contact's chain: carried when it covers what is held, the held one otherwise", () => {
    expect(mergeCarriedProvenPredecessors("cur", [], ["a", "b"])).toEqual(["a", "b"]);
    expect(mergeCarriedProvenPredecessors("cur", ["b"], ["a", "b"])).toEqual(["a", "b"]);
    expect(mergeCarriedProvenPredecessors("cur", ["x"], ["a", "b"])).toEqual(["x"]);
    expect(mergeCarriedProvenPredecessors("cur", [], ["a", "cur", "a"])).toEqual(["a"]);
  });
});

describe("a device paired after a rotation binds under the sender's conversation key", () => {
  it("derives the same conversation account key, and its minted binding names it", async () => {
    // The sending device: rotated twice (seed 1 -> seed 2 -> seed 3).
    const senderKey = await deviceKey();
    let senderState = withPredecessorSeed(EMPTY_KEY_ROTATION_STATE, seed(1));
    senderState = withPredecessorSeed(senderState, seed(2));
    await putKeyRotationState(SENDER, senderState, senderKey);
    const senderConversation = Identity.fromSeed(conversationSeed(senderState) as Uint8Array);
    const current = Identity.fromSeed(seed(3));

    const own = await collectOwnPredecessors(SENDER, senderKey, keyHexOfSeed, 1_700_000_000_000);
    const wire = encodeStateBundle(buildStateBundle([], SENDER, own));

    // The paired device: knows only the current key until the bundle opens.
    const joinerKey = await deviceKey();
    const bundle = decodeStateBundle(wire);
    expect(bundle).not.toBeNull();
    if (bundle === null) return;
    await applyOwnPredecessors(bundle, JOINER, joinerKey, keyHexOfSeed);
    const joinerState = await getKeyRotationState(JOINER, joinerKey);
    const joinerConversation = Identity.fromSeed(conversationSeed(joinerState) as Uint8Array);

    expect(hexOf(joinerConversation.signaturePublicKey)).toBe(hexOf(senderConversation.signaturePublicKey));
    expect(hexOf(joinerConversation.signaturePublicKey)).not.toBe(hexOf(current.signaturePublicKey));

    // The binding the session install mints is signed by that key: the leaf
    // the device publishes resolves to the same account key as the sender's.
    const device = Identity.fromSeedBound(
      new Uint8Array(32).fill(0x44),
      joinerConversation.ed25519Seed,
      BigInt(1_700_000_000),
    );
    expect(hexOf(device.accountPublicKey as Uint8Array)).toBe(hexOf(senderConversation.signaturePublicKey));

    // And the own-key chain the resolver is given names one person.
    const ownKeys = {
      currentKeyB64: base64Encode(current.signaturePublicKey),
      provenPredecessorsB64: predecessorSeeds(joinerState).map((s) =>
        base64Encode(Identity.fromSeed(s).signaturePublicKey),
      ),
    };
    expect(conversationKeyOf(ownKeys)).toBe(base64Encode(senderConversation.signaturePublicKey));
    device.free();
    senderConversation.free();
    joinerConversation.free();
    current.free();
  });
});
