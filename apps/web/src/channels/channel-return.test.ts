// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 section 7 - who sends which channel return, and what the reset
 *  device does with one. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  CHANNEL_RETURN_MAX_SEEDS,
  MAX_HELD_RETURNS,
  channelReturnAction,
  channelReturnsFor,
  dropHeldReturns,
  holdReturn,
  newestSeeds,
  readHeldReturns,
  sentReturnKey,
  unsentReturns,
  type ReturnChannel,
} from "./channel-return";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

function memory(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v), removeItem: (k) => void m.delete(k) };
}

describe("the reset device's action", () => {
  const base = { verified: true, hasInvitation: true, alreadyInRole: false, ownsAlready: false } as const;
  it("an unverified return is shown, never acted on silently and never dropped when it carries an invitation", () => {
    for (const role of ["subscriber", "publisher", "owner"] as const) {
      expect(channelReturnAction({ ...base, verified: false, role })).toBe("show-invitation");
      expect(channelReturnAction({ ...base, verified: false, role, hasInvitation: false })).toBe("drop");
    }
  });
  it("a subscriber subscribes, or only takes the seeds when it already reads", () => {
    expect(channelReturnAction({ ...base, role: "subscriber" })).toBe("subscribe");
    expect(channelReturnAction({ ...base, role: "subscriber", alreadyInRole: true })).toBe("add-seeds");
    expect(channelReturnAction({ ...base, role: "subscriber", hasInvitation: false })).toBe("drop");
  });
  it("a publisher accepts its publisher invitation unless it publishes already", () => {
    expect(channelReturnAction({ ...base, role: "publisher" })).toBe("accept-publisher");
    expect(channelReturnAction({ ...base, role: "publisher", alreadyInRole: true })).toBe("add-seeds");
    expect(channelReturnAction({ ...base, role: "publisher", hasInvitation: false })).toBe("add-seeds");
  });
  it("an owner recovers unless it owns already", () => {
    expect(channelReturnAction({ ...base, role: "owner" })).toBe("recover-owner");
    expect(channelReturnAction({ ...base, role: "owner", ownsAlready: true })).toBe("add-seeds");
  });
});

describe("who sends what", () => {
  const superseded = new Map([
    ["OLD_SUB", "acctSub"],
    ["OLD_PUB", "acctPub"],
    ["OLD_OWNER", "acctOwner"],
  ]);
  const card = (k: string): { identityPkB64: string; subscriberPkB64: string } => ({ identityPkB64: k, subscriberPkB64: `sp-${k}` });
  const ch = (over: Partial<ReturnChannel>): ReturnChannel => ({
    idHex: "c1",
    ownRole: "owner",
    cards: [card("OLD_SUB"), card("OLD_PUB"), card("OLD_OWNER"), card("ME"), card("OTHER")],
    ownerKeyB64: "ME",
    publisherKeysB64: ["OLD_PUB"],
    ...over,
  });

  it("an owner sends subscriber returns and admits publishers", () => {
    const out = channelReturnsFor([ch({ cards: [card("OLD_SUB"), card("OLD_PUB"), card("ME")] })], superseded, new Set(["ME"]));
    expect(out.map((r) => [r.role, r.accountIdB64])).toEqual([
      ["subscriber", "acctSub"],
      ["publisher", "acctPub"],
    ]);
  });
  it("a co-publisher sends subscriber returns, no publisher return, and an owner return for a reset owner", () => {
    const out = channelReturnsFor([ch({ ownRole: "publisher", ownerKeyB64: "OLD_OWNER" })], superseded, new Set(["ME"]));
    expect(out.map((r) => [r.role, r.accountIdB64])).toEqual([
      ["owner", "acctOwner"],
      ["subscriber", "acctSub"],
    ]);
  });
  it("a subscriber sends only an owner return, and only with the owner's card", () => {
    expect(
      channelReturnsFor([ch({ ownRole: "subscriber", ownerKeyB64: "OLD_OWNER" })], superseded, new Set(["ME"])).map(
        (r) => r.role,
      ),
    ).toEqual(["owner"]);
    expect(
      channelReturnsFor(
        [ch({ ownRole: "subscriber", ownerKeyB64: "OLD_OWNER", cards: [card("OLD_SUB")] })],
        superseded,
        new Set(["ME"]),
      ),
    ).toEqual([]);
  });
  it("nothing for this account's own keys", () => {
    expect(
      channelReturnsFor([ch({ cards: [card("OLD_SUB"), card("OLD_PUB")] })], superseded, new Set(["ME", "OLD_SUB", "OLD_PUB"])),
    ).toEqual([]);
  });
  it("each return once per accepted generation", () => {
    const planned = channelReturnsFor([ch({ cards: [card("OLD_SUB"), card("OLD_PUB")] })], superseded, new Set(["ME"]));
    const sent = { [sentReturnKey(planned[0]!)]: 3 };
    const gen = (a: string): number | null => (a === "acctSub" ? 3 : a === "acctPub" ? null : 1);
    expect(unsentReturns(planned, sent, gen)).toEqual([]);
    expect(unsentReturns(planned, sent, () => 4).length).toBe(2);
  });
});

describe("the seeds a return carries", () => {
  it("the newest 64, ascending, one per epoch", () => {
    const seeds = Array.from({ length: 70 }, (_, i) => ({ epoch: i, seed: new Uint8Array(32).fill(i) }));
    seeds.push({ epoch: 5, seed: new Uint8Array(31) });
    const out = newestSeeds(seeds);
    expect(out.epochs.length).toBe(CHANNEL_RETURN_MAX_SEEDS);
    expect(out.epochs[0]).toBe(6);
    expect(out.epochs.at(-1)).toBe(69);
    expect(out.seeds.length).toBe(64 * 32);
    expect(out.seeds[0]).toBe(6);
  });
});

describe("the held returns", () => {
  it("are kept once each, bounded, and dropped when handled", () => {
    const s = memory();
    holdReturn(s, "a", "x");
    holdReturn(s, "a", "x");
    holdReturn(s, "a", "y");
    expect(readHeldReturns(s, "a")).toEqual(["x", "y"]);
    dropHeldReturns(s, "a", ["x"]);
    expect(readHeldReturns(s, "a")).toEqual(["y"]);
    for (let i = 0; i < MAX_HELD_RETURNS + 3; i++) holdReturn(s, "b", String(i));
    expect(readHeldReturns(s, "b").length).toBe(MAX_HELD_RETURNS);
    expect(() => holdReturn(undefined, "a", "x")).toThrow();
  });
});

describe("the wire, through the core", () => {
  it("a return built from a card verifies under the person's previous key and nothing else", () => {
    const old = wasm.Identity.fromSeed(new Uint8Array(32).fill(4));
    const other = wasm.Identity.fromSeed(new Uint8Array(32).fill(5));
    const channelId = new Uint8Array(16).fill(0xc1);
    const sub = wasm.ChannelSubscriberKeypair.generate();
    const subPk = sub.public();
    const seed = new Uint8Array(32).fill(7);
    try {
      const profile = wasm.composeProfileEnvelope(old, "Old name", undefined, 5n);
      const card = wasm.sealMemberCard(seed, 1, channelId, subPk, old.ed25519Seed, profile);
      const opened = wasm.openMemberCard(seed, card, channelId, subPk);
      const envelope = wasm.composeChannelReturnEnvelope(
        channelId,
        wasm.ChannelReturnRole.Subscriber,
        opened.bindingSig,
        subPk,
        opened.profileHash,
        new Uint8Array([1, 2, 3]),
        Uint32Array.from([1]),
        seed,
        profile,
      );
      const ret = wasm.verifyChannelReturnCardBinding(envelope, old.signaturePublicKey);
      expect(ret.role).toBe(wasm.ChannelReturnRole.Subscriber);
      expect(Array.from(ret.epochs)).toEqual([1]);
      // The profile it carries verifies under the same previous key.
      const p = wasm.verifyProfileEnvelope(ret.profile!, old.signaturePublicKey);
      expect(p.displayName).toBe("Old name");
      p.free();
      ret.free();
      expect(() => wasm.verifyChannelReturnCardBinding(envelope, other.signaturePublicKey)).toThrow();
      opened.free();
    } finally {
      old.free();
      other.free();
      sub.free();
    }
  });
});

import { channelsOwingRecoveryKey } from "./channel-return-runtime";

describe("the recovery key registrations owed", () => {
  it("every owned channel not recorded, sorted", () => {
    expect(channelsOwingRecoveryKey(["b", "a", "c"], new Set(["c"]))).toEqual(["a", "b"]);
    expect(channelsOwingRecoveryKey([], new Set())).toEqual([]);
  });
});

import { returnProfileFor } from "./channel-return-runtime";

describe("the profile a return carries", () => {
  it("is the newest raw envelope held under any of the reset account's retired keys", () => {
    const superseded = new Map([
      ["K1", "acct"],
      ["K2", "acct"],
      ["X", "other"],
    ]);
    const held: Record<string, { envelope: Uint8Array; updatedAtMs: bigint }> = {
      K1: { envelope: new Uint8Array([1]), updatedAtMs: 3n },
      K2: { envelope: new Uint8Array([2]), updatedAtMs: 8n },
      X: { envelope: new Uint8Array([9]), updatedAtMs: 99n },
    };
    const source = (k: string): { envelope: Uint8Array; updatedAtMs: bigint } | null => held[k] ?? null;
    expect(Array.from(returnProfileFor("acct", superseded, source) ?? [])).toEqual([2]);
    expect(returnProfileFor("none", superseded, source)).toBeUndefined();
    expect(returnProfileFor("acct", superseded, null)).toBeUndefined();
  });
});
