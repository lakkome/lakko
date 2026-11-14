// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * lakko-security F2 / F3 (2026-11-15), through the REAL wasm core: a
 * malicious Delivery Service raises reset suspicion and replays an old
 * owner-signed Welcome (the KeyPackage is last-resort). A Welcome this device
 * already joined is refused; a new Welcome of a channel founded again is
 * taken; after an inactive group was deleted, a Welcome joining at an epoch
 * not above it is refused (unless the channel is under reset suspicion).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

import { joinReplacingHeldGroup, heldGroupStateFor } from "./publisher-devices";
import {
  JOINED_WELCOMES_KEPT,
  joinBelowDeletedGroup,
  replayGuardOf,
  welcomeAlreadyJoined,
  welcomeHashHex,
  withDeletedGroupEpoch,
  withJoinedWelcome,
} from "./welcome-replay";
import { clearInactiveHeldGroup } from "./channel-entry";
import { decodeAccessRecord, emptyAccessRecord } from "./channel-access-record";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

function deviceOf(account: wasm.Identity, fill: number): wasm.Identity {
  return wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill), account.ed25519Seed, 1_700_000_000n);
}

function admit(g: wasm.PublisherGroup, committer: wasm.Identity, joiner: wasm.Identity): Uint8Array {
  const c = g.stageCommit(committer, [joiner.createKeyPackage()], new Uint8Array(), false);
  const w = c.welcome ?? new Uint8Array();
  c.free();
  g.mergeStagedCommit(committer);
  return w;
}

function targetOf(owner: wasm.Identity, channelId: Uint8Array) {
  return {
    channelIdHex: hex(channelId),
    groupId: wasm.publisherGroupIdForSeed(owner.ed25519Seed, channelId),
    ownerKey: wasm.publisherOwnerAccountKey(owner.ed25519Seed),
    role: "owner" as const,
  };
}

describe("the record of joined Welcomes and the deleted group's epoch", () => {
  it("keeps the newest hashes, bounded, and survives the log codec", async () => {
    let r = emptyAccessRecord();
    const hashes: string[] = [];
    for (let i = 0; i < JOINED_WELCOMES_KEPT + 3; i += 1) {
      const h = await welcomeHashHex(new Uint8Array([i]));
      hashes.push(h);
      r = withJoinedWelcome(r, h);
    }
    expect(r.joinedWelcomeHashes).toHaveLength(JOINED_WELCOMES_KEPT);
    expect(welcomeAlreadyJoined(r, hashes[0] ?? "")).toBe(false);
    expect(welcomeAlreadyJoined(r, hashes.at(-1) ?? "")).toBe(true);
    r = withDeletedGroupEpoch(r, 7n);
    const back = decodeAccessRecord(JSON.parse(JSON.stringify(r)));
    expect(back?.joinedWelcomeHashes).toEqual(r.joinedWelcomeHashes);
    expect(back?.deletedGroupEpoch).toBe(7);
  });

  it("refuses a join at or below the deleted epoch unless the channel is suspected", () => {
    expect(joinBelowDeletedGroup({ joinedEpoch: 2n, deletedGroupEpoch: 2, resetSuspected: false })).toBe(true);
    expect(joinBelowDeletedGroup({ joinedEpoch: 1n, deletedGroupEpoch: 2, resetSuspected: false })).toBe(true);
    expect(joinBelowDeletedGroup({ joinedEpoch: 3n, deletedGroupEpoch: 2, resetSuspected: false })).toBe(false);
    expect(joinBelowDeletedGroup({ joinedEpoch: 1n, deletedGroupEpoch: 2, resetSuspected: true })).toBe(false);
    expect(joinBelowDeletedGroup({ joinedEpoch: 0n, deletedGroupEpoch: undefined, resetSuspected: false })).toBe(false);
  });
});

describe("F2: a replayed Welcome under reset suspicion", () => {
  it("an already-joined Welcome is refused; a re-founded channel's new Welcome is taken", async () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x71));
    const a = deviceOf(owner, 0xd1);
    const b = deviceOf(owner, 0xd2);
    const channelId = new Uint8Array(16).fill(0x72);
    const target = targetOf(owner, channelId);
    const ga = new wasm.PublisherGroup(a, owner.ed25519Seed, channelId);
    try {
      // A admits B (epoch 1); B joins and notes the Welcome.
      const w1 = admit(ga, a, b);
      const first = joinReplacingHeldGroup(wasm, b, owner, target, channelId, w1, false);
      expect(first).not.toBeNull();
      first?.free();
      let record = withJoinedWelcome(emptyAccessRecord(), await welcomeHashHex(w1));
      // A moves on (B rotates with it), the DS raises suspicion and replays w1.
      const w1Again = replayGuardOf(record, await welcomeHashHex(w1));
      expect(w1Again.alreadyJoined).toBe(true);
      expect(joinReplacingHeldGroup(wasm, b, owner, target, channelId, w1, true, undefined, w1Again)).toBeNull();
      // B still holds its group at the epoch it joined.
      const kept = wasm.PublisherGroup.load(b, owner.ed25519Seed, channelId);
      expect(kept?.epoch()).toBe(1n);
      kept?.free();

      // The account's other device founds the channel again (decision A): a
      // NEW Welcome at a low epoch, which B takes under suspicion.
      const c = deviceOf(owner, 0xd3);
      const refounded = new wasm.PublisherGroup(c, owner.ed25519Seed, channelId);
      try {
        const wNew = admit(refounded, c, b);
        const guard = replayGuardOf(record, await welcomeHashHex(wNew));
        expect(guard.alreadyJoined).toBe(false);
        const joined = joinReplacingHeldGroup(wasm, b, owner, target, channelId, wNew, true, undefined, guard);
        expect(joined).not.toBeNull();
        expect(Array.from(joined?.channelSeed(b) ?? [])).toEqual(Array.from(refounded.channelSeed(c)));
        joined?.free();
        record = withJoinedWelcome(record, await welcomeHashHex(wNew));
        expect(record.joinedWelcomeHashes).toHaveLength(2);
      } finally {
        refounded.free();
      }
    } finally {
      ga.free();
    }
  });
});

describe("F3: a join after this device deleted its inactive group", () => {
  it("a Welcome at an epoch not above the deleted group's is refused and leaves nothing; a newer one is taken", async () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x81));
    const a = deviceOf(owner, 0xe1);
    const b = deviceOf(owner, 0xe2);
    const channelId = new Uint8Array(16).fill(0x82);
    const target = targetOf(owner, channelId);
    const ga = new wasm.PublisherGroup(a, owner.ed25519Seed, channelId);
    try {
      // A admits B at epoch 1 (w1); A then retires B (epoch 2); B applies it.
      const w1 = admit(ga, a, b);
      const gb = wasm.PublisherGroup.joinFromWelcome(b, w1, target.groupId, channelId, target.ownerKey);
      const retire = ga.stageRetireDevices(a, b.signaturePublicKey);
      ga.mergeStagedCommit(a);
      gb.processIncomingVerifyingRoles(b, retire).free?.();
      expect(gb.isActive()).toBe(false);
      gb.free();
      // Before an entry the inactive group is deleted; its epoch is recorded.
      const cleared = clearInactiveHeldGroup(wasm, b, owner.ed25519Seed, channelId);
      expect(cleared?.deletedEpoch).toBe(2n);
      expect(heldGroupStateFor(wasm, b, owner, target, channelId)).toBe("none");
      const record = withDeletedGroupEpoch(emptyAccessRecord(), cleared?.deletedEpoch ?? 0n);

      // The DS replays w1 (epoch 1); suppose its hash was lost: the floor
      // refuses it, and nothing of the join is kept.
      const guard = { alreadyJoined: false, deletedGroupEpoch: record.deletedGroupEpoch };
      expect(joinReplacingHeldGroup(wasm, b, owner, target, channelId, w1, false, undefined, guard)).toBeNull();
      expect(heldGroupStateFor(wasm, b, owner, target, channelId)).toBe("none");

      // Under reset suspicion (a channel founded again starts low) it is taken.
      const suspected = joinReplacingHeldGroup(wasm, b, owner, target, channelId, w1, true, undefined, guard);
      expect(suspected).not.toBeNull();
      suspected?.deleteLocally(b);
      suspected?.free();

      // A admits B again (epoch 3): above the floor, taken.
      const w3 = admit(ga, a, b);
      const joined = joinReplacingHeldGroup(wasm, b, owner, target, channelId, w3, false, undefined, guard);
      expect(joined).not.toBeNull();
      expect(joined?.epoch()).toBe(3n);
      joined?.free();
    } finally {
      ga.free();
    }
  });
});
