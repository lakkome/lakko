// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 on web, the wiring: the publisher pass's owned-channels
 * step and the account ports, over the in-memory storage service and the
 * real core, with the channels state and the channels service faked.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { emptyAccessRecord, type ChannelAccessRecord } from "../channel-access-record";

type Identity = InstanceType<typeof wasm.Identity>;

const fake = vi.hoisted(() => ({
  keys: new Map<string, { seed: Uint8Array; signingKeySeed: Uint8Array; seedEpoch?: number }>(),
  epochs: new Map<string, number>(),
  seeds: new Map<string, Map<number, Uint8Array>>(),
  access: new Map<string, unknown>(),
  meta: new Map<string, { listed?: boolean }>(),
  channels: [] as { id: string }[],
  register: new Map<string, { subscribed: boolean; role: string }>(),
  registerLoaded: true,
  recoveryPuts: [] as { idHex: string; pk: number[] }[],
  recoveryStatus: 204 as number | null,
  ownerChanges: [] as { idHex: string; header: string }[],
  ownerStatus: new Map<string, number | null>(),
  subscriberRows: new Map<string, number[]>(),
  rotatedAt: new Map<string, bigint>(),
  wakes: 0,
  owedHandles: [] as { kind: string; id: string; reason: string; rekeyFromSeed?: string }[],
}));

vi.mock("../../chat/handle-owed", () => ({
  openHandleOwed: async () => undefined,
  owedHandleFor: (kind: string, id: string) => fake.owedHandles.find((o) => o.kind === kind && o.id === id),
  recordOwedHandle: async (o: { kind: string; id: string; reason: string; rekeyFromSeed?: string }) => {
    fake.owedHandles = [...fake.owedHandles.filter((x) => !(x.kind === o.kind && x.id === o.id)), o];
  },
}));

vi.mock("../../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));
vi.mock("../../state/channels-state", () => ({
  channelsState: {
    keysByChannel: () => fake.keys,
    epochByChannel: () => fake.epochs,
    subscribedSeedsByChannel: () => fake.seeds,
    metaByChannel: () => fake.meta,
    channels: () => fake.channels,
  },
  channelsActions: {
    accessOf: (id: string) => fake.access.get(id) ?? emptyAccessRecord(),
    updateAccessDurable: async (id: string, f: (r: ChannelAccessRecord) => ChannelAccessRecord) => {
      fake.access.set(id, f((fake.access.get(id) as ChannelAccessRecord | undefined) ?? emptyAccessRecord()));
    },
    addChannel: (c: { id: string }) => void fake.channels.push(c),
    currentSeed: (id: string, epoch: number) => fake.seeds.get(id)?.get(epoch),
    setSubscribedSeedForEpoch: (id: string, epoch: number, seed: Uint8Array) => {
      const inner = fake.seeds.get(id) ?? new Map<number, Uint8Array>();
      inner.set(epoch, seed);
      fake.seeds.set(id, inner);
    },
  },
}));
vi.mock("../api", () => ({
  ownerProofIdentity: (i: unknown) => i,
  putChannelRecoveryKey: async (idHex: string, _owner: unknown, pk: Uint8Array) => {
    fake.recoveryPuts.push({ idHex, pk: Array.from(pk) });
    return fake.recoveryStatus;
  },
  channelOwnerKeyOf: async () => new Uint8Array(32).fill(0x5e),
  putChannelOwnerByRecovery: async (idHex: string, header: string) => {
    fake.ownerChanges.push({ idHex, header });
    return fake.ownerStatus.has(idHex) ? fake.ownerStatus.get(idHex) : 204;
  },
  listSubscriberPage: async (idHex: string, _identity: unknown, after: number) => {
    const rows = (fake.subscriberRows.get(idHex) ?? []).filter((seq) => seq > after);
    return { entries: rows.map((seq) => ({ seq, subscriberId: "", sealedTicket: null })), nextAfter: rows.at(-1) ?? after };
  },
}));
vi.mock("../channel-access", () => ({ channelRotatesHere: (id: string) => fake.keys.has(id) }));
vi.mock("../channel-return-runtime", () => ({ notifyChannelReturnsHeld: () => void (fake.wakes += 1) }));
vi.mock("../channel-subscription-register", () => ({
  channelSubscriptionPort: () =>
    fake.registerLoaded
      ? {
          accountIdB64: "acct",
          get: (id: string) => fake.register.get(id),
          set: async (id: string, v: { role: string }) => {
            fake.register.set(id, { subscribed: true, role: v.role });
            return true;
          },
        }
      : null,
}));
vi.mock("../composer", () => ({
  channelIdFromHex: (hex: string) => Uint8Array.from({ length: 16 }, (_, i) => Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16)),
  freshSubscriberKeys: async () => ({ secret: new Uint8Array(32).fill(1), public: new Uint8Array(32).fill(2) }),
}));
vi.mock("../rotation-state-store", () => ({
  loadRotationState: (id: string) => (fake.rotatedAt.has(id) ? { lastRotationMs: fake.rotatedAt.get(id) } : null),
}));

import { base64Encode } from "../../auth/account-id";
import { isFoundedAlone } from "../founded-alone-marker";
import { readRecoveredChannels } from "../recovered-channels";
import { FakeVaultService } from "./fake-vault-service";
import { readOwedListDeletes, readRecoveryRegistered, readRetake, readRetakenRecordsOwed } from "./owned-facts";
import { hexOf } from "./owned-list";
import {
  addOwnedPasskeyLock,
  clearOwnedChannelsFacts,
  deleteOwnedChannelsVaults,
  dropOwnedChannelsSession,
  ensureOwnedPasswordLock,
  makeOwnedChannelsVaults,
  ownedEntryOf,
  removeOwnedPasskeyLock,
  retakeOwnedChannelsAfterReset,
  runOwnedChannelsPass,
  setOwnedVaultPortForTests,
} from "./owned-runtime";
import { OwnedChannelsStore } from "./owned-store";
import { noteOwnedChannelDeleted } from "./owned-wake";
import { channelHandleSeed } from "../../chat/group-handle";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const bytes = (n: number, fill: number): Uint8Array => new Uint8Array(n).fill(fill);
const ACCOUNT = "acct";
const EXPORT_KEY = bytes(64, 0x11);
const NEW_EXPORT_KEY = bytes(64, 0x12);
const S = bytes(32, 0x22);
const PRF = bytes(32, 0x33);
const TAG = bytes(16, 0x44);
const A = hexOf(bytes(16, 0xa0));
const B = hexOf(bytes(16, 0xb0));
const NOW = 1_800_000_000_000;

let service: FakeVaultService;
let storage: Map<string, string>;
let lineage: Identity;

function own(idHex: string, over: { epoch?: number; access?: Partial<ChannelAccessRecord> } = {}): void {
  const epoch = over.epoch ?? 0;
  fake.keys.set(idHex, { seed: bytes(32, 0x60 + epoch), signingKeySeed: bytes(32, 9), seedEpoch: epoch });
  fake.epochs.set(idHex, epoch);
  fake.access.set(idHex, { ...emptyAccessRecord(), ...over.access });
}

function rotate(idHex: string, epoch: number): void {
  const held = fake.keys.get(idHex);
  if (held === undefined) throw new Error("not owned");
  const kept = fake.seeds.get(idHex) ?? new Map<number, Uint8Array>();
  kept.set(held.seedEpoch ?? 0, held.seed);
  fake.seeds.set(idHex, kept);
  fake.keys.set(idHex, { ...held, seed: bytes(32, 0x60 + epoch), seedEpoch: epoch });
  fake.epochs.set(idHex, epoch);
}

const pass = (): Promise<void> => runOwnedChannelsPass({ accountIdB64: ACCOUNT, lineage, current: lineage });

async function listed(): Promise<{ idHex: string; epochs: number[]; public: boolean; admission: number[] }[]> {
  const store = new OwnedChannelsStore(wasm, service.client());
  const opened = await store.openWithKeys(lineage);
  if (opened === "absent") return [];
  return opened.list.channels.map((c) => ({
    idHex: c.idHex,
    epochs: c.epochs.map((e) => e.epoch),
    public: c.readKey !== null,
    admission: c.admission.map((a) => a.version),
  }));
}

beforeEach(() => {
  service = new FakeVaultService();
  setOwnedVaultPortForTests(service.client());
  storage = new Map();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => void storage.set(k, v),
    removeItem: (k: string) => void storage.delete(k),
  });
  lineage = wasm.Identity.fromSeed(bytes(64, 0xa1));
  for (const m of [fake.keys, fake.epochs, fake.seeds, fake.access, fake.meta, fake.register, fake.ownerStatus, fake.subscriberRows, fake.rotatedAt]) {
    m.clear();
  }
  fake.channels.length = 0;
  fake.recoveryPuts.length = 0;
  fake.ownerChanges.length = 0;
  fake.owedHandles = [];
  fake.recoveryStatus = 204;
  fake.registerLoaded = true;
  fake.wakes = 0;
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  dropOwnedChannelsSession();
  setOwnedVaultPortForTests(null);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const make = (): Promise<"made" | "retry"> =>
  makeOwnedChannelsVaults({ accountIdB64: ACCOUNT, exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage });

describe("the entry this device lists", () => {
  it("a public channel's read key, every admission seed, and only the seeds of epochs the channel reached", () => {
    own(A, { epoch: 2, access: { visibility: "public", readKeyB64: base64Encode(bytes(32, 5)) } });
    fake.seeds.set(A, new Map([[1, bytes(32, 1)]]));
    // A commit whose reseal has not landed: the composer is one epoch ahead.
    fake.keys.set(A, { seed: bytes(32, 0x70), signingKeySeed: bytes(32, 9), seedEpoch: 3 });
    const e = ownedEntryOf(A);
    expect(e.readKey?.[0]).toBe(5);
    expect(e.epochs.map((x) => x.epoch)).toEqual([1]);

    own(B, { access: { visibility: "private", readKeyB64: base64Encode(bytes(32, 5)), admissionSeeds: [{ version: 2, seedB64: base64Encode(bytes(32, 8)) }] } });
    const p = ownedEntryOf(B);
    expect(p.readKey).toBeNull();
    expect(p.admission.map((a) => a.version)).toEqual([2]);
    expect(p.epochs.map((x) => x.epoch)).toEqual([0]);
  });

  it("no read key once the channel's newest record says private, whatever this device's setting", () => {
    own(A, { access: { visibility: "public", readKeyB64: base64Encode(bytes(32, 5)) } });
    fake.meta.set(A, { listed: false });
    expect(ownedEntryOf(A).readKey).toBeNull();
  });
});

describe("the pass on a device with the keys", () => {
  it("registers each owned channel's recovery key from the list key, once, and lists the channel", async () => {
    expect(await make()).toBe("made");
    own(A, { access: { visibility: "public", readKeyB64: base64Encode(bytes(32, 5)) } });
    await pass();
    const store = new OwnedChannelsStore(wasm, service.client());
    const keys = await store.readKeysLock(lineage);
    const expected = wasm.ChannelRecoveryKey.fromListKey(keys?.listKey ?? bytes(32, 0), bytes(16, 0xa0));
    expect(fake.recoveryPuts).toEqual([{ idHex: A, pk: Array.from(expected.verifyingKey()) }]);
    expected.free();
    expect([...readRecoveryRegistered(localStorage, ACCOUNT)]).toEqual([A]);
    expect(await listed()).toEqual([{ idHex: A, epochs: [0], public: true, admission: [] }]);

    // A second pass with nothing changed asks the services for nothing.
    service.seen.length = 0;
    await pass();
    expect(fake.recoveryPuts.length).toBe(1);
    expect(service.seen).toEqual([]);
  });

  it("a refused registration stays owed", async () => {
    await make();
    own(A);
    fake.recoveryStatus = 401;
    await pass();
    expect(readRecoveryRegistered(localStorage, ACCOUNT).size).toBe(0);
    fake.recoveryStatus = 204;
    await pass();
    expect([...readRecoveryRegistered(localStorage, ACCOUNT)]).toEqual([A]);
    expect(fake.recoveryPuts.length).toBe(2);
  });

  it("writes the list after a rotation, a new admission seed and a deletion", async () => {
    await make();
    own(A);
    own(B, { access: { visibility: "private", admissionSeeds: [{ version: 1, seedB64: base64Encode(bytes(32, 7)) }] } });
    await pass();
    for (const epoch of [1, 2, 3, 4, 5]) rotate(A, epoch);
    fake.access.set(B, {
      ...(fake.access.get(B) as ChannelAccessRecord),
      admissionSeeds: [
        { version: 1, seedB64: base64Encode(bytes(32, 7)) },
        { version: 2, seedB64: base64Encode(bytes(32, 8)) },
      ],
    });
    await pass();
    expect(await listed()).toEqual([
      { idHex: A, epochs: [2, 3, 4, 5], public: false, admission: [] },
      { idHex: B, epochs: [0], public: false, admission: [1, 2] },
    ]);

    // The channel is deleted on the service; this device still holds it for
    // a moment (its local wipe follows). The entry leaves the list and is
    // not written back by a pass that runs in between.
    noteOwnedChannelDeleted(ACCOUNT, B);
    expect(readOwedListDeletes(localStorage, ACCOUNT)).toEqual([B]);
    await pass();
    expect((await listed()).map((c) => c.idHex)).toEqual([A]);
    await pass();
    expect((await listed()).map((c) => c.idHex)).toEqual([A]);
    expect(readOwedListDeletes(localStorage, ACCOUNT)).toEqual([B]);
    // The local wipe: the delete is settled.
    fake.keys.delete(B);
    await pass();
    expect((await listed()).map((c) => c.idHex)).toEqual([A]);
    expect(readOwedListDeletes(localStorage, ACCOUNT)).toEqual([]);
  });

  it("a device that owns no channel and owes nothing asks the storage service nothing", async () => {
    await make();
    dropOwnedChannelsSession();
    service.seen.length = 0;
    await pass();
    expect(service.seen).toEqual([]);
    // An owed delete, or an owned channel, is a reason to ask.
    noteOwnedChannelDeleted(ACCOUNT, B);
    await pass();
    expect(service.seen.length).toBeGreaterThan(0);
  });

  it("an account with no keys lock is left alone, and not asked again every pass", async () => {
    own(A);
    await pass();
    expect(fake.recoveryPuts).toEqual([]);
    const asked = service.seen.length;
    expect(asked).toBeGreaterThan(0);
    await pass();
    await pass();
    expect(service.seen.length).toBe(asked);
    expect(service.vaults.size).toBe(0);
    // Once the vaults are made the pass works at once.
    await make();
    await pass();
    expect(fake.recoveryPuts.length).toBe(1);
  });

  it("offline: nothing throws and the next pass does the work", async () => {
    await make();
    dropOwnedChannelsSession();
    own(A);
    service.offline = true;
    await expect(pass()).resolves.toBeUndefined();
    service.offline = false;
    await pass();
    expect((await listed()).map((c) => c.idHex)).toEqual([A]);
  });
});

describe("the account ports", () => {
  it("sign-up makes the vaults; offline answers retry and a later call makes them", async () => {
    service.offline = true;
    expect(await make()).toBe("retry");
    service.offline = false;
    expect(await make()).toBe("made");
    expect(service.heads()).toBe(3);
    expect(await make()).toBe("made");
    expect(service.claims).toBe(3);
  });

  it("a password sign-in on a device with the keys makes the lock again only when it is stale", async () => {
    const input = { accountIdB64: ACCOUNT, lineageIdentity: lineage };
    // An account made before this build: nothing to do, nothing made.
    expect(await ensureOwnedPasswordLock({ ...input, exportKey: EXPORT_KEY })).toBe("absent");
    expect(service.vaults.size).toBe(0);
    await make();
    expect(await ensureOwnedPasswordLock({ ...input, exportKey: EXPORT_KEY })).toBe("current");
    expect(await ensureOwnedPasswordLock({ ...input, exportKey: NEW_EXPORT_KEY })).toBe("made");
    expect(await ensureOwnedPasswordLock({ ...input, exportKey: NEW_EXPORT_KEY })).toBe("current");
    const store = new OwnedChannelsStore(wasm, service.client());
    expect(await store.openWithPassword({ exportKey: EXPORT_KEY, serverHalf: S })).toBe("absent");
    expect(await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S })).not.toBe("absent");
    service.offline = true;
    dropOwnedChannelsSession();
    expect(await ensureOwnedPasswordLock({ ...input, exportKey: EXPORT_KEY })).toBe("retry");
  });

  it("a passkey that keeps the chats gets a lock, and loses it when removed", async () => {
    const input = { accountIdB64: ACCOUNT, lineageIdentity: lineage, credentialTag: TAG };
    expect(await addOwnedPasskeyLock({ ...input, prfOutput: PRF })).toBe("absent");
    expect(await removeOwnedPasskeyLock(input)).toBe("absent");
    await make();
    expect(await addOwnedPasskeyLock({ ...input, prfOutput: PRF })).toBe("added");
    const store = new OwnedChannelsStore(wasm, service.client());
    expect(await store.openWithPasskey({ prfOutput: PRF, serverHalf: S })).not.toBe("absent");
    expect(await removeOwnedPasskeyLock(input)).toBe("removed");
    expect(await store.openWithPasskey({ prfOutput: PRF, serverHalf: S })).toBe("absent");
    expect(await removeOwnedPasskeyLock(input)).toBe("removed");
  });

  it("account deletion deletes the list and every lock", async () => {
    expect(await deleteOwnedChannelsVaults({ lineageIdentity: lineage })).toBe("absent");
    await make();
    await addOwnedPasskeyLock({ accountIdB64: ACCOUNT, lineageIdentity: lineage, credentialTag: TAG, prfOutput: PRF });
    expect(service.vaults.size).toBe(4);
    expect(await deleteOwnedChannelsVaults({ lineageIdentity: lineage })).toBe("deleted");
    expect(service.vaults.size).toBe(0);
    await make();
    service.offline = true;
    expect(await deleteOwnedChannelsVaults({ lineageIdentity: lineage })).toBe("retry");
  });

  it("Forget device clears this account's facts and no other's", async () => {
    await make();
    own(A);
    await pass();
    noteOwnedChannelDeleted(ACCOUNT, B);
    noteOwnedChannelDeleted("other", B);
    clearOwnedChannelsFacts(localStorage, ACCOUNT);
    expect([...storage.keys()]).toEqual(["lakko:owned-channels-deletes:other"]);
    // The list key left memory: the next pass opens the keys lock again.
    service.seen.length = 0;
    await pass();
    expect(service.seen.length).toBeGreaterThan(0);
  });
});

describe("after a key reset", () => {
  async function reset(): Promise<{ next: Identity; listKey: Uint8Array }> {
    await make();
    own(A, { epoch: 3, access: { visibility: "public", readKeyB64: base64Encode(bytes(32, 5)) } });
    own(B, { access: { visibility: "private", admissionSeeds: [{ version: 1, seedB64: base64Encode(bytes(32, 7)) }] } });
    await pass();
    const store = new OwnedChannelsStore(wasm, service.client());
    const listKey = (await store.readKeysLock(lineage))?.listKey ?? bytes(32, 0);
    // The reset: this device starts empty under new keys.
    dropOwnedChannelsSession();
    storage.clear();
    for (const m of [fake.keys, fake.epochs, fake.seeds, fake.access, fake.register]) m.clear();
    fake.channels.length = 0;
    fake.recoveryPuts.length = 0;
    return { next: wasm.Identity.fromSeed(bytes(64, 0xa2)), listKey };
  }

  const retake = (next: Identity): Promise<string> =>
    retakeOwnedChannelsAfterReset({
      accountIdB64: ACCOUNT,
      serverHalf: S,
      sealedBy: { kind: "password", exportKey: EXPORT_KEY },
      newOwner: next,
      newLineageIdentity: next,
      currentExportKey: EXPORT_KEY,
    });

  it("each listed channel is taken with the list key's proof and put on the device from its entry", async () => {
    const { next, listKey } = await reset();
    fake.subscriberRows.set(A, [4, 9]);
    fake.subscriberRows.set(B, [2, 6]);
    vi.spyOn(Date, "now").mockReturnValue(NOW);
    expect(await retake(next)).toBe("done");

    // The owner change of each channel carries the recovery proof of the
    // list key for that channel and the owner key the new account derives
    // (Ed25519 is deterministic and the clock is pinned: the exact header).
    for (const [idHex, fill] of [
      [A, 0xa0],
      [B, 0xb0],
    ] as const) {
      const key = wasm.ChannelRecoveryKey.fromListKey(listKey, bytes(16, fill));
      const expected = key.signOwnerChange(bytes(16, fill), bytes(32, 0x5e), BigInt(NOW));
      key.free();
      expect(fake.ownerChanges.filter((c) => c.idHex === idHex).map((c) => c.header)).toEqual([expected]);
    }

    const a = fake.access.get(A) as ChannelAccessRecord;
    expect(a.visibility).toBe("public");
    expect(a.readKeyB64).toBe(base64Encode(bytes(32, 5)));
    // Public: every reader the service lists stays let in.
    expect(a.admittedThroughSeq).toBe(9);
    expect(a.admissionMarkSet).toBe(true);
    expect(a.admissionSeeds).toEqual([]);
    expect([...(fake.seeds.get(A)?.keys() ?? [])]).toEqual([3]);

    const b = fake.access.get(B) as ChannelAccessRecord;
    expect(b.visibility).toBe("private");
    expect(b.readKeyB64).toBeNull();
    // Private: the rows are judged again by ticket under the listed seeds.
    expect(b.admittedThroughSeq).toBe(0);
    expect(b.admissionMarkSet).toBe(true);
    expect(b.admissionSeeds).toEqual([{ version: 1, seedB64: base64Encode(bytes(32, 7)) }]);

    expect(readRecoveredChannels(ACCOUNT)).toEqual({ [A]: { throughSeq: 9 }, [B]: { throughSeq: 0 } });
    expect(fake.channels.map((c) => c.id).sort()).toEqual([A, B].sort());
    expect(fake.register.get(A)).toEqual({ subscribed: true, role: "owner" });
    expect(fake.register.get(B)).toEqual({ subscribed: true, role: "owner" });
    expect(isFoundedAlone(ACCOUNT)).toBe(true);
    // The publisher pass is woken to found the groups again.
    expect(fake.wakes).toBe(2);
    expect(readRetake(localStorage, ACCOUNT)).toBeNull();

    // The record is owed again only once the group is founded again and the
    // record was read; until then the owed mark is not set (it would be
    // settled with nothing posted).
    lineage = next;
    await pass();
    expect((fake.access.get(A) as ChannelAccessRecord).recordOwed).toBeUndefined();
    expect(readRetakenRecordsOwed(localStorage, ACCOUNT)).toEqual([A, B].sort());
    own(A, { epoch: 4, access: fake.access.get(A) as ChannelAccessRecord });
    await pass();
    expect((fake.access.get(A) as ChannelAccessRecord).recordOwed).toBeUndefined();
    fake.meta.set(A, { listed: true });
    await pass();
    expect((fake.access.get(A) as ChannelAccessRecord).recordOwed).toBe(true);
    expect((fake.access.get(B) as ChannelAccessRecord).recordOwed).toBeUndefined();
    expect(readRetakenRecordsOwed(localStorage, ACCOUNT)).toEqual([B]);
  });

  it("lists each channel's handle seed and owes a rekey of the name from it to the new root's key (item 4)", async () => {
    const id = (hex: string): Uint8Array => Uint8Array.from({ length: 16 }, (_, i) => Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16));
    const oldA = channelHandleSeed(wasm, lineage, lineage, id(A));
    const { next } = await reset();
    expect(await retake(next)).toBe("done");
    const owedA = fake.owedHandles.find((o) => o.id === A);
    expect(owedA?.reason).toBe("rekey");
    expect(owedA?.rekeyFromSeed).toBe(base64Encode(oldA));
    expect(fake.owedHandles.map((o) => o.id).sort()).toEqual([A, B].sort());
    // While the rekey is owed, the list keeps naming the seed the server
    // still holds the name under; once it is not owed, the new root's.
    own(A);
    await runOwnedChannelsPass({ accountIdB64: ACCOUNT, lineage: next, current: next });
    const store = new OwnedChannelsStore(wasm, service.client());
    const opened = await store.openWithKeys(next);
    if (opened === "absent") throw new Error("no keys lock");
    expect(Array.from(opened.list.channels.find((c) => c.idHex === A)?.handleSeed ?? [])).toEqual(Array.from(oldA));
    fake.owedHandles = [];
    dropOwnedChannelsSession();
    await runOwnedChannelsPass({ accountIdB64: ACCOUNT, lineage: next, current: next });
    const again = await store.openWithKeys(next);
    if (again === "absent") throw new Error("no keys lock");
    expect(Array.from(again.list.channels.find((c) => c.idHex === A)?.handleSeed ?? [])).toEqual(
      Array.from(channelHandleSeed(wasm, next, next, id(A))),
    );
  });

  it("a channel whose recovery key is not this list's is dropped; one the service did not answer for is finished by the pass", async () => {
    const { next } = await reset();
    fake.ownerStatus.set(A, 401);
    fake.ownerStatus.set(B, null);
    expect(await retake(next)).toBe("retry");
    expect(fake.channels).toEqual([]);
    expect(readRetake(localStorage, ACCOUNT)).toEqual({ done: [A] });

    // A reload: only the keys are left. The pass continues by itself.
    dropOwnedChannelsSession();
    lineage = next;
    fake.ownerStatus.set(B, 204);
    await pass();
    expect(fake.channels.map((c) => c.id)).toEqual([B]);
    expect(readRetake(localStorage, ACCOUNT)).toBeNull();
    expect((await listed()).map((c) => c.idHex)).toEqual([B]);
    // A was dropped: asked once, never taken.
    expect(fake.ownerChanges.filter((c) => c.idHex === A).length).toBe(1);
  });

  it("waits for the account's register before it counts a channel taken", async () => {
    const { next } = await reset();
    fake.registerLoaded = false;
    expect(await retake(next)).toBe("retry");
    expect(fake.channels).toEqual([]);
    expect(readRetake(localStorage, ACCOUNT)).toEqual({ done: [] });
    fake.registerLoaded = true;
    lineage = next;
    await pass();
    expect(fake.channels.length).toBe(2);
    expect(readRetake(localStorage, ACCOUNT)).toBeNull();
  });

  it("nothing opens with another password: nothing comes back and nothing is owed", async () => {
    const { next } = await reset();
    expect(
      await retakeOwnedChannelsAfterReset({
        accountIdB64: ACCOUNT,
        serverHalf: S,
        sealedBy: { kind: "password", exportKey: NEW_EXPORT_KEY },
        newOwner: next,
        newLineageIdentity: next,
        currentExportKey: NEW_EXPORT_KEY,
      }),
    ).toBe("nothing");
    expect(fake.ownerChanges).toEqual([]);
    expect(readRetake(localStorage, ACCOUNT)).toBeNull();
  });
});
