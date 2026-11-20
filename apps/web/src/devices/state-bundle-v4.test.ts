// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * State bundle v4 (2026-11-13): the newest epoch seeds of the account's
 * channels reach a newly paired device. The golden wire shape of the contract,
 * the newest-32 rule, the size ladder, the decoder's refusals, and the
 * receiver's never-overwrite merge.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { base64Encode } from "../auth/account-id";
import { mergeCarriedSeeds } from "../state/channels-state";
import { MemoryStorage } from "../test/fake-storage";
import {
  MAX_CARRIED_SEED_CHANNELS,
  buildStateBundle,
  carriedChannelSeeds,
  carriedSeedsOf,
  decodeStateBundle,
  encodeStateBundle,
  type StateBundle,
} from "./state-bundle";

beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

const SENDER = "c2VuZGVyLWFjY291bnQtMDA=";
const CHANNEL = "c1".repeat(16);
const MAX_BUNDLE_BYTES = 1 << 20;

const empty: StateBundle = {
  v: 4,
  contacts: [],
  pins: [],
  knownGroupIdsB64: [],
  ownPredecessors: [],
  groupNames: [],
  profiles: [],
  pendingContacts: [],
  channelSeeds: [],
};

function encodeRaw(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

function seedsOf(epochs: readonly number[], fill = 7): Map<number, Uint8Array> {
  return new Map(epochs.map((e) => [e, new Uint8Array(32).fill((e + fill) & 0xff)]));
}

describe("state bundle v4: channel seeds", () => {
  it("carries the contract's golden shape", () => {
    const built = buildStateBundle([], SENDER, [], {
      channelSeeds: new Map([[CHANNEL, new Map([[42, new Uint8Array(32).fill(2)], [41, new Uint8Array(32).fill(1)]])]]),
    });
    expect(built.v).toBe(4);
    expect(built.channelSeeds).toEqual([
      {
        channelIdB64: "wcHBwcHBwcHBwcHBwcHBwQ==",
        seeds: [
          { epoch: 41, seedB64: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=" },
          { epoch: 42, seedB64: "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=" },
        ],
      },
    ]);
    expect(decodeStateBundle(encodeStateBundle(built))).toEqual(built);
  });

  it("keeps the newest 32 per channel, channels in id order", () => {
    const epochs = Array.from({ length: 40 }, (_, i) => i);
    const out = carriedChannelSeeds(
      new Map([
        ["d2".repeat(16), seedsOf([5])],
        [CHANNEL, seedsOf(epochs)],
      ]),
      32,
    );
    expect(out.map((c) => c.channelIdB64)).toEqual([
      base64Encode(new Uint8Array(16).fill(0xc1)),
      base64Encode(new Uint8Array(16).fill(0xd2)),
    ]);
    expect(out[0]?.seeds.map((s) => s.epoch)).toEqual(epochs.slice(8));
  });

  it("lowers the per-channel count for every channel while over the cap; seeds push out nothing", () => {
    const channels = new Map<string, Map<number, Uint8Array>>();
    for (let c = 0; c < 20; c += 1) {
      const idHex = c.toString(16).padStart(2, "0").repeat(16);
      channels.set(idHex, seedsOf(Array.from({ length: 32 }, (_, i) => 1000 + i), c));
    }
    // One profile sized so the bundle WITHOUT seeds is 15 000 bytes under the cap.
    const sizeWith = (n: number): number =>
      encodeStateBundle(
        buildStateBundle([], SENDER, [], {
          profiles: [{ personKeyB64: base64Encode(new Uint8Array(32).fill(3)), envelopeB64: base64Encode(new Uint8Array(n)) }],
        }),
      ).length;
    const probe = 3000;
    const overhead = sizeWith(probe) - Math.ceil(probe / 3) * 4;
    const n = Math.floor(((MAX_BUNDLE_BYTES - 15_000 - overhead) / 4) * 3);
    expect(Math.abs(sizeWith(n) - (MAX_BUNDLE_BYTES - 15_000))).toBeLessThan(10);

    const built = buildStateBundle([], SENDER, [], {
      profiles: [{ personKeyB64: base64Encode(new Uint8Array(32).fill(3)), envelopeB64: base64Encode(new Uint8Array(n)) }],
      channelSeeds: channels,
    });
    expect(built.profiles).toHaveLength(1);
    expect(built.channelSeeds).toHaveLength(20);
    // 16 per channel does not fit in 15 000 bytes, 8 does.
    for (const c of built.channelSeeds) {
      expect(c.seeds.map((s) => s.epoch)).toEqual(Array.from({ length: 8 }, (_, i) => 1024 + i));
    }
    expect(encodeStateBundle(built).length).toBeLessThanOrEqual(MAX_BUNDLE_BYTES);
  });

  it("refuses a v3 bundle and a bundle without the array", () => {
    expect(decodeStateBundle(encodeRaw({ ...empty, v: 3 }))).toBeNull();
    const { channelSeeds: _drop, ...without } = empty;
    expect(decodeStateBundle(encodeRaw(without))).toBeNull();
    expect(decodeStateBundle(encodeRaw(empty))).toEqual(empty);
  });

  it("refuses the whole bundle on any malformed seed entry", () => {
    const id = base64Encode(new Uint8Array(16).fill(0xc1));
    const seed = base64Encode(new Uint8Array(32).fill(1));
    const ok = { channelIdB64: id, seeds: [{ epoch: 1, seedB64: seed }] };
    expect(decodeStateBundle(encodeRaw({ ...empty, channelSeeds: [ok] }))).not.toBeNull();
    const bad: unknown[] = [
      [ok, ok],
      [{ ...ok, channelIdB64: base64Encode(new Uint8Array(15)) }],
      [{ ...ok, seeds: [] }],
      [{ ...ok, seeds: Array.from({ length: 33 }, (_, i) => ({ epoch: i, seedB64: seed })) }],
      [{ ...ok, seeds: [{ epoch: 2, seedB64: seed }, { epoch: 2, seedB64: seed }] }],
      [{ ...ok, seeds: [{ epoch: 3, seedB64: seed }, { epoch: 2, seedB64: seed }] }],
      [{ ...ok, seeds: [{ epoch: 1.5, seedB64: seed }] }],
      [{ ...ok, seeds: [{ epoch: -1, seedB64: seed }] }],
      [{ ...ok, seeds: [{ epoch: 4_294_967_296, seedB64: seed }] }],
      [{ ...ok, seeds: [{ epoch: 1, seedB64: base64Encode(new Uint8Array(31)) }] }],
      [{ ...ok, seeds: [{ epoch: "1", seedB64: seed }] }],
      ["x"],
    ];
    for (const channelSeeds of bad) {
      expect(decodeStateBundle(encodeRaw({ ...empty, channelSeeds }))).toBeNull();
    }
    const tooMany = Array.from({ length: MAX_CARRIED_SEED_CHANNELS + 1 }, (_, i) => ({
      channelIdB64: base64Encode(Uint8Array.from({ length: 16 }, (_, j) => (j < 2 ? (i >> (8 * j)) & 0xff : 0))),
      seeds: [{ epoch: 1, seedB64: seed }],
    }));
    expect(decodeStateBundle(encodeRaw({ ...empty, channelSeeds: tooMany }))).toBeNull();
    expect(
      decodeStateBundle(encodeRaw({ ...empty, channelSeeds: tooMany.slice(0, MAX_CARRIED_SEED_CHANNELS) })),
    ).not.toBeNull();
    // The highest epoch there is.
    expect(
      decodeStateBundle(encodeRaw({ ...empty, channelSeeds: [{ ...ok, seeds: [{ epoch: 4_294_967_295, seedB64: seed }] }] })),
    ).not.toBeNull();
  });

  it("the receiver adds carried seeds and never replaces a held one", () => {
    const decoded = decodeStateBundle(
      encodeStateBundle(
        buildStateBundle([], SENDER, [], {
          channelSeeds: new Map([[CHANNEL, new Map([[1, new Uint8Array(32).fill(1)], [2, new Uint8Array(32).fill(2)]])]]),
        }),
      ),
    );
    expect(decoded).not.toBeNull();
    const carried = carriedSeedsOf(decoded as StateBundle);
    expect([...(carried.get(CHANNEL)?.keys() ?? [])]).toEqual([1, 2]);

    const held = new Map([[CHANNEL, new Map([[2, new Uint8Array(32).fill(9)]])]]);
    const merged = mergeCarriedSeeds(held, carried);
    expect([...(merged?.get(CHANNEL)?.get(1) ?? [])]).toEqual([...new Uint8Array(32).fill(1)]);
    expect([...(merged?.get(CHANNEL)?.get(2) ?? [])]).toEqual([...new Uint8Array(32).fill(9)]);
    // Everything carried is held already: nothing to add.
    expect(mergeCarriedSeeds(merged as Map<string, Map<number, Uint8Array>>, carried)).toBeNull();
  });
});
