// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  decodeStateBundle,
  encodeStateBundle,
  isStateRow,
  unwrapStateRow,
  wrapStateRow,
  type StateBundleV1,
} from "./state-bundle";

const bundle: StateBundleV1 = {
  v: 1,
  contacts: [
    {
      signaturePubkeyB64: "c2ln",
      displayName: "Bob",
      acceptedAtMs: 1_700_000_000_000,
      accountIdHex: "00112233445566778899aabbccddeeff",
    },
  ],
  pins: [{ accountIdB64: "ABEiM0RVZneImaq7zN3u/w==", pinnedSignaturePubkeyB64: "c2ln", rotationMarker: 0 }],
  knownGroupIdsB64: ["Z3JvdXA="],
};

describe("state bundle codec (ADR-0050 section 2.9)", () => {
  it("round-trips", () => {
    expect(decodeStateBundle(encodeStateBundle(bundle))).toEqual(bundle);
  });

  it("refuses a bundle of another version or a malformed contact", () => {
    const other = { ...bundle, v: 2 } as unknown as StateBundleV1;
    expect(decodeStateBundle(encodeStateBundle(other))).toBeNull();
    const broken = new TextEncoder().encode(
      JSON.stringify({ v: 1, contacts: [{ displayName: 1 }], pins: [], knownGroupIdsB64: [] }),
    );
    expect(decodeStateBundle(broken)).toBeNull();
    expect(decodeStateBundle(new Uint8Array([0x7b]))).toBeNull();
  });

  it("tags the mailbox row so the other polls can skip it", () => {
    const sealed = new Uint8Array([1, 2, 3, 4]);
    const row = wrapStateRow(sealed);
    expect(isStateRow(row)).toBe(true);
    expect(unwrapStateRow(row)).toEqual(sealed);
    expect(isStateRow(sealed)).toBe(false);
    expect(unwrapStateRow(sealed)).toBeNull();
  });
});
