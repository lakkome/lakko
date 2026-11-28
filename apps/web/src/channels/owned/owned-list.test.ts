// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 6 on web: what the owned-channels list holds and
 * when this device writes it (create, delete, a rotation, an admission
 * seed, the cap of 64), over the real core, and the facts the device keeps
 * between sessions.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import {
  channelsOwingRecoveryKey,
  clearOwnedChannelsStoredFacts,
  finishRetake,
  markRecoveryRegistered,
  noteRetakeDone,
  oweListDelete,
  oweRetakenRecord,
  readOwedListDeletes,
  readRecoveryRegistered,
  readRetake,
  settleListDeletes,
  startRetake,
  type FactsStorage,
} from "./owned-facts";
import {
  MAX_ADMISSION_SEEDS,
  MAX_EPOCH_SEEDS,
  MAX_OWNED_CHANNELS,
  decodeOwnedList,
  emptyOwnedList,
  encodeOwnedList,
  hexOf,
  idsBeyondCap,
  listCovers,
  listWritePlan,
  sameList,
  trimEntry,
  withChannelChanges,
  type OwnedEntry,
  type OwnedList,
} from "./owned-list";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const bytes = (n: number, fill: number): Uint8Array => new Uint8Array(n).fill(fill);
const idOf = (n: number): string => hexOf(Uint8Array.from({ length: 16 }, (_, i) => (i === 15 ? n & 0xff : i === 14 ? n >> 8 : 0)));
const never = (): number => 0;

function entry(n: number, over: Partial<OwnedEntry> = {}): OwnedEntry {
  return { idHex: idOf(n), readKey: null, admission: [], epochs: [], ...over };
}
const epochs = (...ids: number[]): OwnedEntry["epochs"] => ids.map((epoch) => ({ epoch, seed: bytes(32, epoch) }));
const admission = (...versions: number[]): OwnedEntry["admission"] =>
  versions.map((version) => ({ version, seed: bytes(32, version) }));

function fresh(): OwnedList {
  return emptyOwnedList(bytes(32, 0x77));
}
const roundTrip = (list: OwnedList): OwnedList => decodeOwnedList(wasm, encodeOwnedList(wasm, list));

describe("one entry", () => {
  it("keeps the newest four epoch seeds and the eight highest admission seeds", () => {
    const e = trimEntry(entry(1, { epochs: epochs(9, 2, 5, 7, 3, 8), admission: admission(1, 2, 3, 4, 5, 6, 7, 8, 9, 10) }));
    expect(e.epochs.map((x) => x.epoch)).toEqual([5, 7, 8, 9]);
    expect(e.epochs.length).toBe(MAX_EPOCH_SEEDS);
    expect(e.admission.map((x) => x.version)).toEqual([3, 4, 5, 6, 7, 8, 9, 10]);
    expect(e.admission.length).toBe(MAX_ADMISSION_SEEDS);
  });

  it("keeps two admission seeds of one version, each once, and one seed per epoch", () => {
    const e = trimEntry(
      entry(1, {
        admission: [
          { version: 2, seed: bytes(32, 9) },
          { version: 2, seed: bytes(32, 4) },
          { version: 2, seed: bytes(32, 9) },
        ],
        epochs: [
          { epoch: 3, seed: bytes(32, 1) },
          { epoch: 3, seed: bytes(32, 2) },
        ],
      }),
    );
    expect(e.admission.map((a) => a.seed[0])).toEqual([4, 9]);
    expect(e.epochs.map((x) => x.seed[0])).toEqual([1]);
  });

  it("drops a seed or a read key of the wrong length", () => {
    const e = trimEntry(
      entry(1, { readKey: bytes(31, 1), epochs: [{ epoch: 1, seed: bytes(31, 1) }], admission: [{ version: 1, seed: bytes(33, 1) }] }),
    );
    expect(e).toEqual(entry(1, { handleSeed: null }));
  });
});

describe("the list over the core", () => {
  it("round-trips a list with locks, a public and a private channel", () => {
    const list: OwnedList = {
      passwordLockRoot: bytes(32, 1),
      passkeyLocks: [{ tagHex: hexOf(bytes(16, 2)), root: bytes(32, 3) }],
      channels: [
        entry(1, { readKey: bytes(32, 5), epochs: epochs(1, 2) }),
        entry(2, { admission: admission(1, 2), epochs: epochs(4) }),
      ],
    };
    const back = roundTrip(list);
    expect(sameList(back, list)).toBe(true);
    expect(encodeOwnedList(wasm, list).length).toBe(wasm.ownedChannelsListLen());
  });

  it("a created channel enters the list and a deleted one leaves it", () => {
    let list = withChannelChanges(wasm, fresh(), [entry(1, { epochs: epochs(0) }), entry(2)], [], never);
    expect(list.channels.map((c) => c.idHex)).toEqual([idOf(1), idOf(2)]);
    list = withChannelChanges(wasm, list, [], [idOf(1)], never);
    expect(list.channels.map((c) => c.idHex)).toEqual([idOf(2)]);
    // A deleted channel does not come back with an entry written in the same
    // change.
    list = withChannelChanges(wasm, list, [entry(2, { epochs: epochs(3) })], [idOf(2)], never);
    expect(list.channels).toEqual([]);
  });

  it("a rotation adds the new epoch seed and keeps the newest four", () => {
    let list = withChannelChanges(wasm, fresh(), [entry(1, { epochs: epochs(1, 2, 3, 4) })], [], never);
    list = withChannelChanges(wasm, list, [entry(1, { epochs: epochs(5) })], [], never);
    expect(list.channels[0]?.epochs.map((e) => e.epoch)).toEqual([2, 3, 4, 5]);
    // The seeds another device listed stay when this one knows fewer.
    list = withChannelChanges(wasm, list, [entry(1, { epochs: epochs(6) })], [], never);
    expect(list.channels[0]?.epochs.map((e) => e.epoch)).toEqual([3, 4, 5, 6]);
    expect(roundTrip(list).channels[0]?.epochs.length).toBe(4);
  });

  it("this device's seed wins an epoch both name", () => {
    let list = withChannelChanges(wasm, fresh(), [entry(1, { epochs: [{ epoch: 4, seed: bytes(32, 1) }] })], [], never);
    list = withChannelChanges(wasm, list, [entry(1, { epochs: [{ epoch: 4, seed: bytes(32, 2) }] })], [], never);
    expect(list.channels[0]?.epochs.map((e) => e.seed[0])).toEqual([2]);
  });

  it("a minted admission seed is added and the eight highest kept", () => {
    let list = withChannelChanges(wasm, fresh(), [entry(1, { admission: admission(1, 2, 3, 4, 5, 6, 7, 8) })], [], never);
    list = withChannelChanges(wasm, list, [entry(1, { admission: admission(9) })], [], never);
    expect(list.channels[0]?.admission.map((a) => a.version)).toEqual([2, 3, 4, 5, 6, 7, 8, 9]);
  });

  it("a channel change leaves the lock roots alone", () => {
    const list: OwnedList = {
      passwordLockRoot: bytes(32, 1),
      passkeyLocks: [{ tagHex: hexOf(bytes(16, 2)), root: bytes(32, 3) }],
      channels: [],
    };
    const next = withChannelChanges(wasm, list, [entry(1)], [], never);
    expect(Array.from(next.passwordLockRoot)).toEqual(Array.from(list.passwordLockRoot));
    expect(next.passkeyLocks.map((l) => l.tagHex)).toEqual([hexOf(bytes(16, 2))]);
  });
});

describe("the handle write seed (list v2)", () => {
  it("round-trips over the core and is listed only at 32 bytes", () => {
    const list = withChannelChanges(wasm, fresh(), [entry(1, { handleSeed: bytes(32, 9) }), entry(2)], [], never);
    const back = roundTrip(list);
    expect(Array.from(back.channels.find((c) => c.idHex === idOf(1))?.handleSeed ?? [])).toEqual(
      Array.from(bytes(32, 9)),
    );
    expect(back.channels.find((c) => c.idHex === idOf(2))?.handleSeed).toBeNull();
    expect(trimEntry(entry(3, { handleSeed: bytes(31, 1) })).handleSeed).toBeNull();
    expect(encodeOwnedList(wasm, back).length).toBe(65_532);
  });

  it("this device's seed replaces the listed one; none keeps the listed one", () => {
    let list = withChannelChanges(wasm, fresh(), [entry(1, { handleSeed: bytes(32, 1) })], [], never);
    list = withChannelChanges(wasm, list, [entry(1, { epochs: epochs(2) })], [], never);
    expect(list.channels[0]?.handleSeed?.[0]).toBe(1);
    list = withChannelChanges(wasm, list, [entry(1, { handleSeed: bytes(32, 2) })], [], never);
    expect(list.channels[0]?.handleSeed?.[0]).toBe(2);
  });

  it("a changed seed is a write; the same seed is none", () => {
    const list = withChannelChanges(wasm, fresh(), [entry(1, { handleSeed: bytes(32, 1) })], [], never);
    expect(listWritePlan(list, [entry(1, { handleSeed: bytes(32, 1) })], [], never).entries).toEqual([]);
    expect(listWritePlan(list, [entry(1, { handleSeed: bytes(32, 3) })], [], never).entries.length).toBe(1);
    expect(listCovers(list, entry(1))).toBe(true);
  });
});

describe("the cap of 64", () => {
  it("drops the least recently rotated, a tie by id", () => {
    const ids = Array.from({ length: 66 }, (_, i) => idOf(i + 1));
    const rotatedAt = (id: string): number => (id === idOf(1) ? 50 : id === idOf(2) ? 0 : 100);
    // idOf(2) was never rotated; idOf(1) rotated before every other.
    expect(idsBeyondCap(ids, rotatedAt).sort()).toEqual([idOf(1), idOf(2)]);
    expect(idsBeyondCap(ids.slice(0, MAX_OWNED_CHANNELS), rotatedAt)).toEqual([]);
    // All equal: the highest ids go, the same on every device.
    expect(idsBeyondCap(ids, never).sort()).toEqual([idOf(65), idOf(66)]);
  });

  it("an account owning more than 64 lists the 64 it rotated most recently", () => {
    const full = withChannelChanges(
      wasm,
      fresh(),
      Array.from({ length: 64 }, (_, i) => entry(i + 1)),
      [],
      never,
    );
    expect(full.channels.length).toBe(64);
    // A 65th, rotated just now, pushes out the one rotated longest ago.
    const rotatedAt = (id: string): number => (id === idOf(65) ? 1000 : id === idOf(7) ? 1 : 500);
    const next = withChannelChanges(wasm, full, [entry(65, { epochs: epochs(1) })], [], rotatedAt);
    expect(next.channels.length).toBe(64);
    expect(next.channels.some((c) => c.idHex === idOf(65))).toBe(true);
    expect(next.channels.some((c) => c.idHex === idOf(7))).toBe(false);
    expect(encodeOwnedList(wasm, next).length).toBe(wasm.ownedChannelsListLen());
    // A 65th never rotated does not enter.
    const kept = withChannelChanges(wasm, full, [entry(65)], [], (id) => (id === idOf(65) ? 0 : 500));
    expect(kept.channels.some((c) => c.idHex === idOf(65))).toBe(false);
  });
});

describe("what this device has to write", () => {
  const listed = (): OwnedList =>
    withChannelChanges(
      wasm,
      fresh(),
      [entry(1, { epochs: epochs(1, 2), readKey: bytes(32, 6) }), entry(2, { admission: admission(1) })],
      [],
      never,
    );

  it("nothing when the list already says everything", () => {
    const list = listed();
    const plan = listWritePlan(
      list,
      [entry(1, { epochs: epochs(2), readKey: bytes(32, 6) }), entry(2, { admission: admission(1) })],
      [],
      never,
    );
    expect(plan).toEqual({ entries: [], deletes: [] });
    expect(listCovers(list, entry(1, { epochs: epochs(1), readKey: bytes(32, 6) }))).toBe(true);
    // The same seeds with no read key say something else: the channel is
    // not public any more.
    expect(listCovers(list, entry(1, { epochs: epochs(1) }))).toBe(false);
  });

  it("a new channel, a new epoch seed, a new admission seed and a changed read key", () => {
    const list = listed();
    const wants = (e: OwnedEntry): string[] => listWritePlan(list, [e], [], never).entries.map((x) => x.idHex);
    expect(wants(entry(3))).toEqual([idOf(3)]);
    expect(wants(entry(1, { epochs: epochs(3), readKey: bytes(32, 6) }))).toEqual([idOf(1)]);
    expect(wants(entry(2, { admission: admission(1, 2) }))).toEqual([idOf(2)]);
    expect(wants(entry(1, { readKey: bytes(32, 8) }))).toEqual([idOf(1)]);
    // A channel that lists no read key and now has one (it went public).
    expect(wants(entry(2, { admission: admission(1), readKey: bytes(32, 8) }))).toEqual([idOf(2)]);
  });

  it("a public channel that went private loses its listed read key", () => {
    const list = listed();
    const private1 = entry(1, { epochs: epochs(2), admission: admission(1) });
    expect(listWritePlan(list, [private1], [], never).entries.map((e) => e.idHex)).toEqual([idOf(1)]);
    const next = withChannelChanges(wasm, list, [private1], [], never);
    const written = next.channels.find((c) => c.idHex === idOf(1));
    expect(written?.readKey).toBeNull();
    // The seeds listed before stay.
    expect(written?.epochs.map((e) => e.epoch)).toEqual([1, 2]);
    expect(roundTrip(next).channels.find((c) => c.idHex === idOf(1))?.readKey).toBeNull();
    expect(listWritePlan(next, [private1], [], never).entries).toEqual([]);
  });

  it("an older epoch seed the list dropped is not written back", () => {
    const list = withChannelChanges(wasm, fresh(), [entry(1, { epochs: epochs(5, 6, 7, 8) })], [], never);
    expect(listWritePlan(list, [entry(1, { epochs: epochs(2, 8) })], [], never).entries).toEqual([]);
  });

  it("an owed delete is written while the list still carries the channel, and never re-added", () => {
    const list = listed();
    const plan = listWritePlan(list, [entry(1, { epochs: epochs(9) }), entry(3)], [idOf(1), idOf(9)], never);
    expect(plan.deletes).toEqual([idOf(1)]);
    expect(plan.entries.map((e) => e.idHex)).toEqual([idOf(3)]);
  });

  it("a channel past the cap is not written again every pass", () => {
    const full = withChannelChanges(wasm, fresh(), Array.from({ length: 64 }, (_, i) => entry(i + 1)), [], never);
    const plan = listWritePlan(full, [entry(65)], [], (id) => (id === idOf(65) ? 0 : 500));
    expect(plan.entries).toEqual([]);
  });
});

describe("the facts kept between sessions", () => {
  function memory(): FactsStorage & { keys(): string[] } {
    const m = new Map<string, string>();
    return {
      getItem: (k) => m.get(k) ?? null,
      setItem: (k, v) => void m.set(k, v),
      removeItem: (k) => void m.delete(k),
      keys: () => [...m.keys()].sort(),
    };
  }

  it("a channel registered under the removed OPRF key owes its registration again", () => {
    const s = memory();
    // The row of 2026-11-25.
    s.setItem("lakko:channel-recovery-registered:acct", JSON.stringify([idOf(1)]));
    expect(channelsOwingRecoveryKey([idOf(1), idOf(2)], readRecoveryRegistered(s, "acct"))).toEqual([idOf(1), idOf(2)]);
    markRecoveryRegistered(s, "acct", idOf(1));
    expect(channelsOwingRecoveryKey([idOf(1), idOf(2)], readRecoveryRegistered(s, "acct"))).toEqual([idOf(2)]);
    expect(readRecoveryRegistered(s, "other").size).toBe(0);
  });

  it("an owed delete is kept until the list write lands", () => {
    const s = memory();
    oweListDelete(s, "acct", idOf(1));
    oweListDelete(s, "acct", idOf(2).toUpperCase());
    expect(readOwedListDeletes(s, "acct")).toEqual([idOf(1), idOf(2)]);
    settleListDeletes(s, "acct", [idOf(1)]);
    expect(readOwedListDeletes(s, "acct")).toEqual([idOf(2)]);
    settleListDeletes(s, "acct", [idOf(2)]);
    expect(s.keys()).toEqual([]);
  });

  it("a retake is owed from its start to its finish, and remembers the entries settled", () => {
    const s = memory();
    expect(readRetake(s, "acct")).toBeNull();
    startRetake(s, "acct");
    noteRetakeDone(s, "acct", idOf(3));
    startRetake(s, "acct");
    expect(readRetake(s, "acct")).toEqual({ done: [idOf(3)] });
    finishRetake(s, "acct");
    expect(readRetake(s, "acct")).toBeNull();
    noteRetakeDone(s, "acct", idOf(4));
    expect(readRetake(s, "acct")).toBeNull();
    expect(() => startRetake(undefined, "acct")).toThrow();
  });

  it("the wipe removes every fact of the account and no other account's", () => {
    const s = memory();
    for (const acct of ["a", "b"]) {
      markRecoveryRegistered(s, acct, idOf(1));
      oweListDelete(s, acct, idOf(2));
      startRetake(s, acct);
      oweRetakenRecord(s, acct, idOf(3));
      s.setItem(`lakko:channel-recovery-registered:${acct}`, "[]");
    }
    clearOwnedChannelsStoredFacts(s, "a");
    expect(s.keys().every((k) => k.endsWith(":b"))).toBe(true);
    expect(s.keys().length).toBe(5);
    expect(() => clearOwnedChannelsStoredFacts(undefined, "a")).not.toThrow();
  });
});
