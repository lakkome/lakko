// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel-log codec: what survives a decode, what is dropped, and
 * that an encode round-trips. These are the rules a reload depends on —
 * a subscriber's seed or a creator's signing seed that decodes wrong
 * is a channel that silently stops reading or signs under a new key.
 */

import { describe, it, expect } from "vitest";
import {
  CHANNEL_LOG_VERSION,
  bytesToHex,
  decodeChannelLog,
  emptyChannelLogSnapshot,
  encodeChannelLog,
  hexToBytesExact,
  type ChannelLogPayload,
  type ChannelLogSnapshot,
} from "./channel-log-codec";

const CH_A = "0123456789abcdef0123456789abcdef";
const CH_B = "fedcba9876543210fedcba9876543210";

function bytes(fill: number, len = 32): Uint8Array {
  return new Uint8Array(len).fill(fill);
}

function sample(): ChannelLogSnapshot {
  return {
    channels: [
      { id: CH_A, localName: "Alpha", localDescription: "first", addedAtMs: 1000, localHandle: "alpha" },
      { id: CH_B, localName: "", localDescription: "", addedAtMs: 2000 },
    ],
    subscriberKeys: new Map([[CH_B, { secret: bytes(0x11), public: bytes(0x22) }]]),
    subscribedSeeds: new Map([
      [
        CH_B,
        new Map([
          [3, bytes(0x33)],
          [4, bytes(0x44)],
        ]),
      ],
    ]),
    publisherSigningSeeds: new Map([[CH_A, bytes(0x55)]]),
    channelMeta: new Map([
      [CH_A, { name: "Alpha news", description: "the first\nchannel", updatedAtMs: 18_446_744_073_709_551_615n }],
    ]),
    publisherProviderSnapshot: new Uint8Array([0, 0, 0, 1, 9, 8, 7]),
    publisherLeafKeyB64: "bGVhZg==",
    coPublisherGroupIds: new Map([[CH_B, "ab".repeat(32)]]),
    publisherCommits: [
      {
        id: "0f".repeat(16),
        kind: "rotation",
        channelIdHex: CH_A,
        groupIdHex: "cd".repeat(32),
        targetEpoch: "7",
        serverTargetEpoch: 3,
        commitB64: null,
        proofEpoch: "6",
        proofSeedB64: "c2VlZA==",
        journalLost: false,
        welcomeB64: null,
        joinerKeyB64: null,
        replyGrantB64: null,
        done: ["journal"],
      },
    ],
  };
}

describe("channel log codec", () => {
  it("round-trips every section through encode -> decode", () => {
    const decoded = decodeChannelLog(encodeChannelLog(sample()));
    expect(decoded).not.toBeNull();
    const d = decoded!;
    expect(d.channels).toEqual(sample().channels);
    expect(d.subscriberKeys.get(CH_B)?.secret).toEqual(bytes(0x11));
    expect(d.subscriberKeys.get(CH_B)?.public).toEqual(bytes(0x22));
    expect(d.subscribedSeeds.get(CH_B)?.get(3)).toEqual(bytes(0x33));
    expect(d.subscribedSeeds.get(CH_B)?.get(4)).toEqual(bytes(0x44));
    expect(d.publisherSigningSeeds.get(CH_A)).toEqual(bytes(0x55));
    // ADR-0051 — the stamp is a u64 and must come back exact.
    expect(d.channelMeta.get(CH_A)).toEqual({
      name: "Alpha news",
      description: "the first\nchannel",
      updatedAtMs: 18_446_744_073_709_551_615n,
    });
    expect(d.publisherProviderSnapshot).toEqual(new Uint8Array([0, 0, 0, 1, 9, 8, 7]));
    expect(d.publisherLeafKeyB64).toBe("bGVhZg==");
    // ADR-0078 section 6 + G-50.
    expect(d.coPublisherGroupIds.get(CH_B)).toBe("ab".repeat(32));
    expect(d.publisherCommits).toEqual(sample().publisherCommits);
  });

  it("drops a malformed co-publisher id or commit entry alone", () => {
    const wire = encodeChannelLog(sample());
    const back = decodeChannelLog({
      ...wire,
      coPublisherGroupIds: [...(wire.coPublisherGroupIds ?? []), { channelId: CH_A, groupIdHex: "zz" }],
      publisherCommits: [...(wire.publisherCommits ?? []), { id: "short", kind: "rotation" }],
    })!;
    expect(back.coPublisherGroupIds.size).toBe(1);
    expect(back.publisherCommits).toHaveLength(1);
  });

  it("encodes the documented v1 wire shape", () => {
    const wire = encodeChannelLog(sample());
    expect(wire.version).toBe(CHANNEL_LOG_VERSION);
    expect(wire.channels[0]).toEqual({
      id: CH_A,
      localName: "Alpha",
      localDescription: "first",
      addedAtMs: 1000,
      localHandle: "alpha",
    });
    // A channel with no handle carries no `localHandle` key at all.
    expect(Object.keys(wire.channels[1]!)).toEqual(["id", "localName", "localDescription", "addedAtMs"]);
    expect(wire.subscriberKeys).toEqual([
      { channelId: CH_B, secretHex: "11".repeat(32), publicHex: "22".repeat(32) },
    ]);
    expect(wire.subscribedSeeds).toEqual([
      { channelId: CH_B, epoch: 3, seedHex: "33".repeat(32) },
      { channelId: CH_B, epoch: 4, seedHex: "44".repeat(32) },
    ]);
    expect(wire.publisherSigningSeeds).toEqual([{ channelId: CH_A, seedHex: "55".repeat(32) }]);
    expect(wire.channelMeta).toEqual([
      {
        channelId: CH_A,
        name: "Alpha news",
        description: "the first\nchannel",
        updatedAtMs: "18446744073709551615",
      },
    ]);
    // JSON-safe: no Uint8Array / Map reaches the wire.
    expect(JSON.parse(JSON.stringify(wire))).toEqual(wire);
  });

  it("an empty snapshot encodes to empty sections and a null provider snapshot", () => {
    const wire = encodeChannelLog(emptyChannelLogSnapshot());
    expect(wire).toEqual({
      version: 1,
      channels: [],
      subscriberKeys: [],
      subscribedSeeds: [],
      publisherSigningSeeds: [],
      channelMeta: [],
      publisherProviderSnapshotB64: null,
      publisherLeafKeyB64: null,
      coPublisherGroupIds: [],
      publisherCommits: [],
      channelAccess: [],
    });
    const back = decodeChannelLog(wire)!;
    expect(back.channels).toEqual([]);
    expect(back.publisherProviderSnapshot).toBeNull();
    expect(back.publisherLeafKeyB64).toBeNull();
  });

  it("refuses an unknown version and non-object payloads outright", () => {
    expect(decodeChannelLog({ ...encodeChannelLog(sample()), version: 2 })).toBeNull();
    expect(decodeChannelLog({ channels: [] })).toBeNull();
    expect(decodeChannelLog(null)).toBeNull();
    expect(decodeChannelLog("nope")).toBeNull();
    expect(decodeChannelLog(42)).toBeNull();
  });

  it("drops a malformed entry alone and keeps the rest", () => {
    const wire: ChannelLogPayload = {
      version: 1,
      channels: [
        { id: "short", localName: "x", localDescription: "", addedAtMs: 1 },
        { id: CH_A, localName: "ok", localDescription: "", addedAtMs: 1 },
        { id: CH_A, localName: "dup", localDescription: "", addedAtMs: 2 },
        // @ts-expect-error -- a non-numeric addedAtMs is exactly the malformed row under test
        { id: CH_B, localName: "bad", localDescription: "", addedAtMs: "1" },
      ],
      subscriberKeys: [
        // 31-byte secret: never padded into a key.
        { channelId: CH_A, secretHex: "11".repeat(31), publicHex: "22".repeat(32) },
        { channelId: CH_B, secretHex: "11".repeat(32), publicHex: "22".repeat(32) },
        { channelId: CH_B.toUpperCase(), secretHex: "11".repeat(32), publicHex: "22".repeat(32) },
      ],
      subscribedSeeds: [
        { channelId: CH_B, epoch: -1, seedHex: "33".repeat(32) },
        { channelId: CH_B, epoch: 1.5, seedHex: "33".repeat(32) },
        { channelId: CH_B, epoch: 2, seedHex: "zz".repeat(32) },
        { channelId: CH_B, epoch: 2, seedHex: "33".repeat(32) },
      ],
      publisherSigningSeeds: [
        { channelId: CH_A, seedHex: "55".repeat(33) },
        { channelId: CH_B, seedHex: "66".repeat(32) },
      ],
      channelMeta: [
        { channelId: CH_A, name: "ok", description: "", updatedAtMs: "7" },
        // A negative / non-decimal stamp is a malformed record, never 0.
        { channelId: CH_B, name: "bad", description: "", updatedAtMs: "-7" },
        // @ts-expect-error -- a numeric stamp is exactly the malformed row under test
        { channelId: CH_B, name: "bad", description: "", updatedAtMs: 7 },
      ],
    };
    const d = decodeChannelLog(wire)!;
    expect(d.channels.map((c) => c.id)).toEqual([CH_A]);
    expect(d.channels[0]?.localName).toBe("ok");
    expect([...d.subscriberKeys.keys()]).toEqual([CH_B]);
    expect([...(d.subscribedSeeds.get(CH_B)?.keys() ?? [])]).toEqual([2]);
    expect([...d.publisherSigningSeeds.keys()]).toEqual([CH_B]);
    expect([...d.channelMeta.keys()]).toEqual([CH_A]);
    expect(d.channelMeta.get(CH_A)?.updatedAtMs).toBe(7n);
  });

  it("tolerates missing sections (a row written by a narrower writer)", () => {
    const d = decodeChannelLog({ version: 1 })!;
    expect(d.channels).toEqual([]);
    expect(d.subscriberKeys.size).toBe(0);
    expect(d.subscribedSeeds.size).toBe(0);
    expect(d.publisherSigningSeeds.size).toBe(0);
    expect(d.channelMeta.size).toBe(0);
  });

  it("a provider snapshot without its leaf key is dropped, and vice versa", () => {
    const base = encodeChannelLog(emptyChannelLogSnapshot());
    const noKey = decodeChannelLog({ ...base, publisherProviderSnapshotB64: "AAAA", publisherLeafKeyB64: null })!;
    expect(noKey.publisherProviderSnapshot).toBeNull();
    expect(noKey.publisherLeafKeyB64).toBeNull();
    const noSnap = decodeChannelLog({ ...base, publisherProviderSnapshotB64: null, publisherLeafKeyB64: "k" })!;
    expect(noSnap.publisherProviderSnapshot).toBeNull();
    expect(noSnap.publisherLeafKeyB64).toBeNull();
    const badB64 = decodeChannelLog({ ...base, publisherProviderSnapshotB64: "%%%", publisherLeafKeyB64: "k" })!;
    expect(badB64.publisherProviderSnapshot).toBeNull();
  });

  it("hex helpers are strict and exact", () => {
    expect(bytesToHex(new Uint8Array([0, 15, 255]))).toBe("000fff");
    expect(hexToBytesExact("000fff", 6)).toEqual(new Uint8Array([0, 15, 255]));
    expect(hexToBytesExact("000FFF", 6)).toEqual(new Uint8Array([0, 15, 255]));
    expect(hexToBytesExact("000fff", 4)).toBeNull();
    expect(hexToBytesExact("00gfff", 6)).toBeNull();
    expect(hexToBytesExact(123, 6)).toBeNull();
  });
});
