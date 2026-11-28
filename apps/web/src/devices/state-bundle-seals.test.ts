// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Alignment of 2026-11-27, item F: the pairing state bundle carries each
 * contact's seal-announcement floor (`sealFloors`, ADR-0101 section 10) and
 * the keys a contact left behind by an unsealed reset (`startedOver`,
 * section 11). Additive inside v4, absent when empty, the same JSON on the
 * three clients; the receiver keeps the higher `seq` and adds a left-behind
 * key it does not have.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  accountIdB64OfHex,
  bindContactsAccount,
  loadStartedOverKeys,
  raiseSealSeqFloor,
  recordStartedOver,
  sealSeqFloorOf,
  setPin,
  upsertAccepted,
} from "../contacts/local-store";
import { applyStateBundle, buildStateBundle, decodeStateBundle, encodeStateBundle } from "./state-bundle";

const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";
const ANNA_HEX = "0123456789abcdef0123456789abcdef";
const ANNA = accountIdB64OfHex(ANNA_HEX) as string;
const OLD = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const NEW = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

/** The shared vector, byte for byte the same text on the three clients. */
const SHARED_VECTOR =
  '{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],' +
  '"profiles":[],"pendingContacts":[],"channelSeeds":[],' +
  `"sealFloors":[{"accountIdHex":"${ANNA_HEX}","seq":7}],` +
  `"startedOver":[{"accountIdHex":"${ANNA_HEX}","oldKeyB64":"${OLD}","atMs":1700000000000}]}`;

beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
  bindContactsAccount(OWNER);
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

describe("sealFloors and startedOver in the pairing state bundle", () => {
  it("decodes the shared vector", () => {
    const b = decodeStateBundle(new TextEncoder().encode(SHARED_VECTOR));
    expect(b?.sealFloors).toEqual([{ accountIdHex: ANNA_HEX, seq: 7 }]);
    expect(b?.startedOver).toEqual([{ accountIdHex: ANNA_HEX, oldKeyB64: OLD, atMs: 1700000000000 }]);
  });

  it("an older bundle without them still decodes; a malformed entry refuses the bundle", () => {
    const plain =
      '{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],' +
      '"profiles":[],"pendingContacts":[],"channelSeeds":[]}';
    const b = decodeStateBundle(new TextEncoder().encode(plain));
    expect(b).not.toBeNull();
    expect(b?.sealFloors).toBeUndefined();
    for (const bad of [
      SHARED_VECTOR.replace('"seq":7', '"seq":-1'),
      SHARED_VECTOR.replace('"seq":7', '"seq":"7"'),
      SHARED_VECTOR.replace(`"accountIdHex":"${ANNA_HEX}","seq"`, '"accountIdHex":"zz","seq"'),
      SHARED_VECTOR.replace(`"oldKeyB64":"${OLD}"`, '"oldKeyB64":"AQ=="'),
      SHARED_VECTOR.replace('"atMs":1700000000000', '"atMs":1.5'),
    ]) {
      expect(decodeStateBundle(new TextEncoder().encode(bad))).toBeNull();
    }
  });

  it("the sending device carries what it holds; the receiving one keeps the higher seq and adds the key", () => {
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    upsertAccepted({ signaturePubkeyB64: OLD, displayName: "Anna", acceptedAtMs: 1, accountIdHex: ANNA_HEX });
    raiseSealSeqFloor(ANNA, 9n);
    recordStartedOver({
      accountIdB64: ANNA,
      heldKeyB64: OLD,
      newKeyB64: NEW,
      keyGeneration: 2,
      signedAtMs: 1,
      nowMs: 1_700_000_000_000,
    });
    const built = buildStateBundle([], OWNER);
    expect(built.sealFloors).toEqual([{ accountIdHex: ANNA_HEX, seq: 9 }]);
    expect(built.startedOver).toEqual([{ accountIdHex: ANNA_HEX, oldKeyB64: OLD, atMs: 1_700_000_000_000 }]);
    const wire = encodeStateBundle(built);

    // A fresh device of the same account: it holds the pin with a lower floor.
    (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
    bindContactsAccount(OWNER);
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    raiseSealSeqFloor(ANNA, 3n);
    const decoded = decodeStateBundle(wire);
    if (decoded === null) throw new Error("refused");
    applyStateBundle(decoded, OWNER);
    expect(sealSeqFloorOf(ANNA)).toBe(9n);
    expect(loadStartedOverKeys().get(OLD)).toBe(ANNA);
    // A higher floor held here is never lowered.
    raiseSealSeqFloor(ANNA, 20n);
    applyStateBundle(decoded, OWNER);
    expect(sealSeqFloorOf(ANNA)).toBe(20n);
  });
});
