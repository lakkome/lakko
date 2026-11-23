// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0080 amendment (2026-11-23): the pairing state bundle carries the
 * confirming device's device cards. The shared vector below is the same JSON
 * the iOS (`StateBundleDeviceCardsTests.swift`) and Android
 * (`StateBundleDeviceCardsTest.kt`) suites decode, with the same expected
 * outcome; the envelopes are real `composeDeviceCardEnvelope` output.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { MemoryStorage } from "../test/fake-storage";
import { applyStateBundleDeviceCards, decodeCarriedDeviceCards } from "./carried-device-cards";
import { acceptCarriedDeviceCards, cardsStorageKey, type StoredDeviceCard } from "./device-cards";
import {
  MAX_CARRIED_DEVICE_CARDS,
  buildStateBundle,
  carriedDeviceCards,
  decodeStateBundle,
  encodeStateBundle,
} from "./state-bundle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

const KEY_1 = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const KEY_2 = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const HEX_1 = "01".repeat(32);
const HEX_2 = "02".repeat(32);
const HEX_OWN = "03".repeat(32);
/** iOS card: iPhone18,4, Lakko 1.0, iOS 26.4, no place, stamp 1700000000000. */
const ENVELOPE_IOS =
  "bGFra28vZGV2aWNlLWNhcmQvdjEAAQAAAYvP5WgAAAABi8/la+gBCmlQaG9uZTE4LDQDMS4wCGlPUyAyNi40AAA=";
/** Web card: Chrome on macOS, Almere NL, stamp 1700000002000. */
const ENVELOPE_WEB =
  "bGFra28vZGV2aWNlLWNhcmQvdjEAAQAAAYvP5W/QAAABi8/lc7gFD0Nocm9tZSBvbiBtYWNPUwN3ZWIFbWFjT1MGQWxtZXJlAk5M";

/** The shared vector, byte for byte the same text on the three clients. */
const SHARED_VECTOR =
  '{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],' +
  '"profiles":[],"pendingContacts":[],"channelSeeds":[],"deviceCards":[' +
  `{"leafKeyB64":"${KEY_1}","envelopeB64":"${ENVELOPE_IOS}"},` +
  `{"leafKeyB64":"${KEY_2}","envelopeB64":"${ENVELOPE_WEB}"}]}`;

const EXPECTED_IOS: StoredDeviceCard = {
  platform: "ios",
  model: "iPhone18,4",
  appVersion: "1.0",
  os: "iOS 26.4",
  city: "",
  country: "",
  updatedAtMs: 1700000000000,
  lastActiveMs: 1700000001000,
};
const EXPECTED_WEB: StoredDeviceCard = {
  platform: "web",
  model: "Chrome on macOS",
  appVersion: "web",
  os: "macOS",
  city: "Almere",
  country: "NL",
  updatedAtMs: 1700000002000,
  lastActiveMs: 1700000003000,
};

function vectorBytes(): Uint8Array {
  return new TextEncoder().encode(SHARED_VECTOR);
}

function roster(keysHex: readonly string[], ownHex: string): { devices(): unknown[]; ownLeafIndex(): number } {
  const hexBytes = (h: string): Uint8Array => Uint8Array.from(h.match(/../g)!.map((b) => Number.parseInt(b, 16)));
  return {
    devices: () => keysHex.map((k, i) => ({ leafIndex: i, signaturePublicKey: hexBytes(k) })),
    ownLeafIndex: () => keysHex.indexOf(ownHex),
  };
}

describe("state bundle device cards: the shared vector", () => {
  it("decodes the two entries and both envelopes decode to the expected cards", () => {
    const bundle = decodeStateBundle(vectorBytes());
    expect(bundle).not.toBeNull();
    expect(bundle?.deviceCards).toEqual([
      { leafKeyB64: KEY_1, envelopeB64: ENVELOPE_IOS },
      { leafKeyB64: KEY_2, envelopeB64: ENVELOPE_WEB },
    ]);
    expect(decodeCarriedDeviceCards(wasm, bundle?.deviceCards ?? [])).toEqual([
      { keyHex: HEX_1, card: EXPECTED_IOS },
      { keyHex: HEX_2, card: EXPECTED_WEB },
    ]);
  });

  it("the vector's envelopes are what the core composes", () => {
    const card = new wasm.DeviceCard(1700000000000n, 1700000001000n, "ios", "iPhone18,4", "1.0", "iOS 26.4", "", "");
    expect(Buffer.from(wasm.composeDeviceCardEnvelope(card)).toString("base64")).toBe(ENVELOPE_IOS);
    card.free();
  });

  it("the receiver stores both for current leaves of its roster", () => {
    const bundle = decodeStateBundle(vectorBytes());
    const kept = applyStateBundleDeviceCards(wasm, bundle!, "acct", roster([HEX_1, HEX_2, HEX_OWN], HEX_OWN));
    expect(kept).toBe(2);
    const row = JSON.parse(localStorage.getItem(cardsStorageKey("acct")) ?? "null");
    expect(row.cards).toEqual({ [HEX_1]: EXPECTED_IOS, [HEX_2]: EXPECTED_WEB });
  });

  it("drops a card for a key that is not in the roster, and never its own", () => {
    const bundle = decodeStateBundle(vectorBytes());
    expect(applyStateBundleDeviceCards(wasm, bundle!, "acct", roster([HEX_1, HEX_OWN], HEX_OWN))).toBe(1);
    expect(applyStateBundleDeviceCards(wasm, bundle!, "acct2", roster([HEX_1, HEX_2], HEX_2))).toBe(1);
    const row = JSON.parse(localStorage.getItem(cardsStorageKey("acct2")) ?? "null");
    expect(Object.keys(row.cards)).toEqual([HEX_1]);
  });
});

describe("state bundle device cards: the store rule", () => {
  it("keeps only a strictly newer card, so the device's own later card wins", () => {
    const newer = { ...EXPECTED_IOS, updatedAtMs: EXPECTED_IOS.updatedAtMs + 1, model: "newer" };
    const held = { [HEX_1]: newer };
    expect(acceptCarriedDeviceCards(held, [{ keyHex: HEX_1, card: EXPECTED_IOS }], [HEX_1], HEX_OWN)).toBeNull();
    expect(acceptCarriedDeviceCards(held, [{ keyHex: HEX_1, card: newer }], [HEX_1], HEX_OWN)).toBeNull();
    const older = { [HEX_1]: { ...EXPECTED_IOS, updatedAtMs: 0 } };
    expect(acceptCarriedDeviceCards(older, [{ keyHex: HEX_1, card: EXPECTED_IOS }], [HEX_1], HEX_OWN)).toEqual({
      [HEX_1]: EXPECTED_IOS,
    });
  });
});

describe("state bundle device cards: the codec", () => {
  it("an older bundle without the field decodes, and an empty list is not written", () => {
    const built = buildStateBundle([], "acct");
    const text = new TextDecoder().decode(encodeStateBundle(built));
    expect(text.includes("deviceCards")).toBe(false);
    expect(decodeStateBundle(encodeStateBundle(built))?.deviceCards).toBeUndefined();
  });

  it("the builder orders by key bytes, keeps the first per key, and bounds the count", () => {
    const keys = Array.from({ length: 10 }, (_, i) => Buffer.from(new Uint8Array(32).fill(10 - i)).toString("base64"));
    const entries = [...keys, keys[0] as string].map((k) => ({ leafKeyB64: k, envelopeB64: ENVELOPE_WEB }));
    const out = carriedDeviceCards(entries);
    expect(out.length).toBe(MAX_CARRIED_DEVICE_CARDS);
    expect(out[0]?.leafKeyB64).toBe(Buffer.from(new Uint8Array(32).fill(1)).toString("base64"));
    const built = buildStateBundle([], "acct", [], { deviceCards: entries });
    expect(decodeStateBundle(encodeStateBundle(built))?.deviceCards).toEqual(out);
  });

  it("refuses the whole bundle on a bad entry, a repeated key, or too many", () => {
    const withCards = (cards: unknown): Uint8Array => {
      const o = JSON.parse(SHARED_VECTOR) as Record<string, unknown>;
      o["deviceCards"] = cards;
      return new TextEncoder().encode(JSON.stringify(o));
    };
    expect(decodeStateBundle(withCards("x"))).toBeNull();
    expect(decodeStateBundle(withCards([{ leafKeyB64: "AQ==", envelopeB64: ENVELOPE_IOS }]))).toBeNull();
    expect(decodeStateBundle(withCards([{ leafKeyB64: KEY_1, envelopeB64: "AAAA" }]))).toBeNull();
    expect(
      decodeStateBundle(
        withCards([
          { leafKeyB64: KEY_1, envelopeB64: ENVELOPE_IOS },
          { leafKeyB64: KEY_1, envelopeB64: ENVELOPE_WEB },
        ]),
      ),
    ).toBeNull();
    const nine = Array.from({ length: 9 }, (_, i) => ({
      leafKeyB64: Buffer.from(new Uint8Array(32).fill(i + 1)).toString("base64"),
      envelopeB64: ENVELOPE_WEB,
    }));
    expect(decodeStateBundle(withCards(nine))).toBeNull();
  });
});
