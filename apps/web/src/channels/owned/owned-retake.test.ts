// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 8 on web: after a key reset the account takes its
 * channels back from its list. The per-entry decisions, the locks made and
 * deleted, and the resume after an interruption, against the in-memory
 * storage service and the real core; the channels side is a fake port.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { FakeVaultService } from "./fake-vault-service";
import { readRetake, type FactsStorage } from "./owned-facts";
import { hexOf, withChannelChanges, type OwnedEntry } from "./owned-list";
import {
  continueRetake,
  retakeAfterReset,
  retakeVerdict,
  retakenChannel,
  type RetakeInput,
  type RetakePorts,
  type RetakenChannel,
} from "./owned-retake";
import { OwnedChannelsStore } from "./owned-store";

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
const C = hexOf(bytes(16, 0xc0));

const identity = (fill: number): InstanceType<typeof wasm.Identity> => wasm.Identity.fromSeed(bytes(64, fill));
const OLD = (): InstanceType<typeof wasm.Identity> => identity(0xa1);
const NEW = (): InstanceType<typeof wasm.Identity> => identity(0xa2);

function memory(): FactsStorage {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v), removeItem: (k) => void m.delete(k) };
}

function vaultIdOf(root: Uint8Array): string {
  const keys = wasm.vaultKeysFromRoot(root);
  const ref = wasm.VaultRef.ownedChannels();
  try {
    return hexOf(keys.vaultId(ref));
  } finally {
    ref.free();
    keys.free();
  }
}

const ENTRIES: OwnedEntry[] = [
  { idHex: A, readKey: bytes(32, 5), admission: [], epochs: [{ epoch: 3, seed: bytes(32, 3) }] },
  { idHex: B, readKey: null, admission: [{ version: 1, seed: bytes(32, 7) }], epochs: [] },
  { idHex: C, readKey: null, admission: [], epochs: [] },
];

/** An account with three listed channels, made before the reset. */
async function account(): Promise<{ service: FakeVaultService; store: OwnedChannelsStore; listKey: Uint8Array }> {
  const service = new FakeVaultService();
  const store = new OwnedChannelsStore(wasm, service.client());
  const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: OLD() });
  await store.writeList(made.listKey, (l) => withChannelChanges(wasm, l, ENTRIES, [], () => 0));
  return { service, store, listKey: made.listKey };
}

interface FakeChannels extends RetakePorts {
  readonly status: Map<string, number | null>;
  readonly ownerChanges: { idHex: string; listKey: number[] }[];
  readonly taken: { idHex: string; plan: RetakenChannel }[];
  takeFails: boolean;
}

function channels(status: Record<string, number | null>): FakeChannels {
  const fake: FakeChannels = {
    status: new Map(Object.entries(status)),
    ownerChanges: [],
    taken: [],
    takeFails: false,
    async changeOwner(idHex, listKey) {
      fake.ownerChanges.push({ idHex, listKey: Array.from(listKey) });
      return fake.status.get(idHex) ?? null;
    },
    async takeChannel(entry, plan) {
      if (fake.takeFails) throw new Error("the register is not loaded yet");
      fake.taken.push({ idHex: entry.idHex, plan });
    },
  };
  return fake;
}

const passwordSealed = (over: Partial<RetakeInput> = {}): RetakeInput => ({
  accountIdB64: ACCOUNT,
  serverHalf: S,
  sealedBy: { kind: "password", exportKey: EXPORT_KEY },
  newLineageIdentity: NEW(),
  currentExportKey: EXPORT_KEY,
  ...over,
});

describe("one owner change", () => {
  it("204 takes the channel, 401 / 404 / 410 drop the entry, anything else is tried again", () => {
    expect(retakeVerdict(204)).toBe("owned");
    for (const s of [401, 404, 410]) expect(retakeVerdict(s)).toBe("drop");
    for (const s of [null, 400, 403, 409, 429, 500, 503]) expect(retakeVerdict(s)).toBe("retry");
  });

  it("a listed read key says public; listed admission seeds and no read key say private, judged by ticket", () => {
    expect(retakenChannel(ENTRIES[0] as OwnedEntry)).toEqual({ visibility: "public", admission: "everyone-listed" });
    expect(retakenChannel(ENTRIES[1] as OwnedEntry)).toEqual({ visibility: "private", admission: "tickets" });
    expect(retakenChannel(ENTRIES[2] as OwnedEntry)).toEqual({ visibility: null, admission: "everyone-listed" });
    // A public channel that was private once keeps its seeds and stays public.
    expect(retakenChannel({ ...(ENTRIES[0] as OwnedEntry), admission: [{ version: 1, seed: bytes(32, 1) }] })).toEqual({
      visibility: "public",
      admission: "everyone-listed",
    });
  });
});

describe("a reset sealed by the password", () => {
  it("takes the channels that answer 204, drops the one that answers 401, and finishes the rest later", async () => {
    const { service, store, listKey } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204, [B]: 401, [C]: null });

    const first = await retakeAfterReset(wasm, store, storage, passwordSealed(), ports);
    expect(first.outcome).toBe("retry");
    expect(ports.taken.map((t) => t.idHex)).toEqual([A]);
    expect(ports.taken[0]?.plan).toEqual({ visibility: "public", admission: "everyone-listed" });
    // The recovery proof is the list key's: every owner change was signed
    // under R, which this device opened from the password lock.
    expect(ports.ownerChanges.every((c) => hexOf(Uint8Array.from(c.listKey)) === hexOf(listKey))).toBe(true);
    // The dropped entry left the list; the one still owed stays.
    expect((await store.readList(listKey))?.list.channels.map((c) => c.idHex)).toEqual([A, C]);
    expect(readRetake(storage, ACCOUNT)).toEqual({ done: [A, B].sort() });

    // The new keys lock holds R and S; the old one stays for a recall device.
    const byNewKeys = await store.readKeysLock(NEW());
    expect(hexOf(byNewKeys?.listKey ?? bytes(0, 0))).toBe(hexOf(listKey));
    expect(hexOf(byNewKeys?.serverHalf ?? bytes(0, 0))).toBe(hexOf(S));
    expect(await store.readKeysLock(OLD())).not.toBeNull();

    // Later, with nothing but its own keys (no S, no sealing secret).
    ports.status.set(C, 204);
    const second = await retakeAfterReset(
      wasm,
      store,
      storage,
      passwordSealed({ serverHalf: null, sealedBy: null, currentExportKey: null }),
      ports,
    );
    expect(second.outcome).toBe("done");
    expect(ports.taken.map((t) => t.idHex)).toEqual([A, C]);
    // A is not asked again.
    expect(ports.ownerChanges.filter((c) => c.idHex === A).length).toBe(1);
    expect(readRetake(storage, ACCOUNT)).toBeNull();
    // The password lock was never touched: the reset kept the password.
    expect(await store.passwordLockIsStale(EXPORT_KEY, S)).toBe(false);
    expect(service.vaults.size).toBe(4);
  });

  it("a channel that could not be put on this device yet is tried again, not counted taken", async () => {
    const { store, listKey } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204, [B]: 204, [C]: 204 });
    ports.takeFails = true;
    expect((await retakeAfterReset(wasm, store, storage, passwordSealed(), ports)).outcome).toBe("retry");
    expect(readRetake(storage, ACCOUNT)).toEqual({ done: [] });
    expect((await store.readList(listKey))?.list.channels.length).toBe(3);

    // The publisher pass continues from the keys lock alone.
    ports.takeFails = false;
    const opened = await store.openWithKeys(NEW());
    if (opened === "absent") throw new Error("the new keys lock did not open");
    const continued = await continueRetake(wasm, store, storage, ACCOUNT, opened, ports);
    expect(continued.outcome).toBe("done");
    expect(ports.taken.map((t) => t.idHex).sort()).toEqual([A, B, C].sort());
    expect(readRetake(storage, ACCOUNT)).toBeNull();
    // With no retake owed, nothing is asked.
    const asked = ports.ownerChanges.length;
    expect((await continueRetake(wasm, store, storage, ACCOUNT, opened, ports)).outcome).toBe("done");
    expect(ports.ownerChanges.length).toBe(asked);
  });

  it("deletes the passkey locks the list names: the reset deleted the passkeys", async () => {
    const { service, store, listKey } = await account();
    await store.addPasskeyLock({ listKey, serverHalf: S, credentialTag: TAG, prfOutput: PRF });
    const passkeyVault = vaultIdOf(wasm.ownedPasskeyLockRoot(PRF, S));
    expect(service.vaults.has(passkeyVault)).toBe(true);
    const ports = channels({ [A]: 204, [B]: 204, [C]: 204 });
    expect((await retakeAfterReset(wasm, store, memory(), passwordSealed(), ports)).outcome).toBe("done");
    expect(service.vaults.has(passkeyVault)).toBe(false);
    expect((await store.readList(listKey))?.list.passkeyLocks).toEqual([]);
  });
});

describe("a reset sealed by a passkey, after the password changed without the keys", () => {
  it("opens the passkey lock, makes the password lock again, and deletes the passkey locks", async () => {
    const { service, store, listKey } = await account();
    await store.addPasskeyLock({ listKey, serverHalf: S, credentialTag: TAG, prfOutput: PRF });
    const oldPasswordVault = vaultIdOf(wasm.ownedPasswordLockRoot(EXPORT_KEY, S));
    const newPasswordVault = vaultIdOf(wasm.ownedPasswordLockRoot(NEW_EXPORT_KEY, S));
    const passkeyVault = vaultIdOf(wasm.ownedPasskeyLockRoot(PRF, S));
    // The new password names no lock, and does not open the list.
    expect(await store.passwordLockIsStale(NEW_EXPORT_KEY, S)).toBe(true);
    expect(await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S })).toBe("absent");

    const storage = memory();
    const ports = channels({ [A]: 204, [B]: 204, [C]: 204 });
    const result = await retakeAfterReset(
      wasm,
      store,
      storage,
      {
        accountIdB64: ACCOUNT,
        serverHalf: S,
        sealedBy: { kind: "passkey", prfOutput: PRF, credentialTag: TAG },
        newLineageIdentity: NEW(),
        currentExportKey: NEW_EXPORT_KEY,
      },
      ports,
    );
    expect(result.outcome).toBe("done");
    expect(ports.taken.map((t) => t.idHex).sort()).toEqual([A, B, C].sort());
    expect(ports.taken.find((t) => t.idHex === B)?.plan).toEqual({ visibility: "private", admission: "tickets" });
    expect(ports.ownerChanges.every((c) => hexOf(Uint8Array.from(c.listKey)) === hexOf(listKey))).toBe(true);

    expect(service.vaults.has(newPasswordVault)).toBe(true);
    expect(service.vaults.has(oldPasswordVault)).toBe(false);
    expect(service.vaults.has(passkeyVault)).toBe(false);
    const list = (await store.readList(listKey))?.list;
    expect(hexOf(list?.passwordLockRoot ?? bytes(0, 0))).toBe(hexOf(wasm.ownedPasswordLockRoot(NEW_EXPORT_KEY, S)));
    expect(list?.passkeyLocks).toEqual([]);
    // The list opens with the new password and with the new keys now.
    expect(await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S })).not.toBe("absent");
    expect(await store.openWithKeys(NEW())).not.toBe("absent");
    expect(await store.openWithPasskey({ prfOutput: PRF, serverHalf: S })).toBe("absent");
  });
});

describe("nothing to take back, and the network", () => {
  it("no lock opens: nothing is owed and no keys lock is made", async () => {
    const { service, store } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204 });
    const result = await retakeAfterReset(
      wasm,
      store,
      storage,
      passwordSealed({ sealedBy: { kind: "password", exportKey: NEW_EXPORT_KEY } }),
      ports,
    );
    expect(result.outcome).toBe("nothing");
    expect(ports.ownerChanges).toEqual([]);
    expect(readRetake(storage, ACCOUNT)).toBeNull();
    expect(await store.readKeysLock(NEW())).toBeNull();
    expect(service.vaults.size).toBe(3);
  });

  it("offline: the retake stays owed, nothing is taken, and a later call does it all", async () => {
    const { service, store } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204, [B]: 204, [C]: 204 });
    service.offline = true;
    const first = await retakeAfterReset(wasm, store, storage, passwordSealed(), ports);
    expect(first.outcome).toBe("retry");
    expect(ports.ownerChanges).toEqual([]);
    expect(readRetake(storage, ACCOUNT)).toEqual({ done: [] });
    service.offline = false;
    expect((await retakeAfterReset(wasm, store, storage, passwordSealed(), ports)).outcome).toBe("done");
    expect(ports.taken.length).toBe(3);
  });

  it("without the server half and before any keys lock exists there is nothing to continue from", async () => {
    const { store } = await account();
    const ports = channels({ [A]: 204 });
    const result = await retakeAfterReset(wasm, store, memory(), passwordSealed({ serverHalf: null }), ports);
    expect(result.outcome).toBe("retry");
    expect(ports.ownerChanges).toEqual([]);
  });

  it("an unsealed reset opens nothing: nothing is owed", async () => {
    const { store } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204 });
    const result = await retakeAfterReset(wasm, store, storage, passwordSealed({ sealedBy: null }), ports);
    expect(result.outcome).toBe("nothing");
    expect(readRetake(storage, ACCOUNT)).toBeNull();
    expect(ports.ownerChanges).toEqual([]);
  });
});

describe("a recall (ADR-0074) through the old keys lock", () => {
  it("opens the list with the old keys lock's root, makes the new keys lock and takes every channel", async () => {
    const { store, listKey } = await account();
    const storage = memory();
    const ports = channels({ [A]: 204, [B]: 204, [C]: 204 });
    const root = wasm.ownedKeysLockRoot(OLD());
    const result = await retakeAfterReset(
      wasm,
      store,
      storage,
      { ...passwordSealed({ sealedBy: null, serverHalf: null, currentExportKey: NEW_EXPORT_KEY }), oldKeysRoot: root },
      ports,
    );
    expect(result.outcome).toBe("done");
    expect(ports.taken.map((t) => t.idHex).sort()).toEqual([A, B, C].sort());
    expect(ports.ownerChanges.every((c) => c.listKey.join() === Array.from(listKey).join())).toBe(true);
    const keys = await store.readKeysLock(NEW());
    expect(Array.from(keys?.listKey ?? [])).toEqual(Array.from(listKey));
    expect(Array.from(keys?.serverHalf ?? [])).toEqual(Array.from(S));
    // The new password gets its lock.
    expect(await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S })).not.toBe("absent");
  });

  it("an old keys lock that is not there brings nothing back", async () => {
    const { store } = await account();
    const storage = memory();
    const result = await retakeAfterReset(
      wasm,
      store,
      storage,
      { ...passwordSealed({ sealedBy: null }), oldKeysRoot: new Uint8Array(32).fill(9) },
      channels({}),
    );
    expect(result.outcome).toBe("nothing");
    expect(readRetake(storage, ACCOUNT)).toBeNull();
  });
});
