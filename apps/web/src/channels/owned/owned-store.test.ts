// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 (items 2 to 9) on web: the owned-channels list and its
 * locks against a storage service that enforces version + 1 and answers
 * `409 vault-version-conflict` (`fake-vault-service.ts`, behind the real
 * `VaultClient`), with the real core.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { FakeVaultService } from "./fake-vault-service";
import { encodeOwnedList, hexOf, withChannelChanges, type OwnedEntry, type OwnedList } from "./owned-list";
import { LIST_WRITE_ATTEMPTS, OwnedChannelsStore, OwnedVaultError } from "./owned-store";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const bytes = (n: number, fill: number): Uint8Array => new Uint8Array(n).fill(fill);
const EXPORT_KEY = bytes(64, 0x11);
const NEW_EXPORT_KEY = bytes(64, 0x12);
const S = bytes(32, 0x22);
const PRF = bytes(32, 0x33);
const TAG = bytes(16, 0x44);

function lineage(fill = 0xa1): InstanceType<typeof wasm.Identity> {
  return wasm.Identity.fromSeed(bytes(64, fill));
}

function entry(idFill: number, epochs: number[] = [], readKey: Uint8Array | null = null): OwnedEntry {
  return {
    idHex: hexOf(bytes(16, idFill)),
    readKey,
    admission: [],
    epochs: epochs.map((epoch) => ({ epoch, seed: bytes(32, epoch) })),
  };
}

function setup(): { service: FakeVaultService; store: OwnedChannelsStore } {
  const service = new FakeVaultService();
  return { service, store: new OwnedChannelsStore(wasm, service.client()) };
}

/** The vault id (hex) of the vault a 32-byte root names. */
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

/** Seal `list` as the head of the list vault at `version`, as another
 *  device of the account would. */
function sealList(listKey: Uint8Array, version: bigint, list: OwnedList): Uint8Array {
  const keys = wasm.vaultKeysFromRoot(listKey);
  const ref = wasm.VaultRef.ownedChannels();
  try {
    return keys.sealHead(ref, version, encodeOwnedList(wasm, list));
  } finally {
    ref.free();
    keys.free();
  }
}

const add =
  (...entries: OwnedEntry[]) =>
  (current: OwnedList): OwnedList =>
    withChannelChanges(wasm, current, entries, [], () => 0);

describe("making the three vaults (item 4)", () => {
  it("makes the keys lock, the list and the password lock, and a second call writes nothing", async () => {
    const { service, store } = setup();
    const id = lineage();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    expect(service.claims).toBe(3);
    expect(service.heads()).toBe(3);
    expect(made.listKey.length).toBe(32);
    expect(Array.from(made.serverHalf ?? [])).toEqual(Array.from(S));
    expect(made.list.channels).toEqual([]);
    expect(made.list.passkeyLocks).toEqual([]);
    expect(Array.from(made.list.passwordLockRoot)).toEqual(Array.from(wasm.ownedPasswordLockRoot(EXPORT_KEY, S)));
    // The three vaults are the ones the roots name.
    expect(service.vaults.has(vaultIdOf(wasm.ownedKeysLockRoot(id)))).toBe(true);
    expect(service.vaults.has(vaultIdOf(wasm.ownedPasswordLockRoot(EXPORT_KEY, S)))).toBe(true);
    expect(service.vaults.has(vaultIdOf(made.listKey))).toBe(true);

    const writesBefore = service.seen.filter((r) => r.method !== "GET").length;
    const again = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    expect(service.seen.filter((r) => r.method !== "GET").length).toBe(writesBefore);
    expect(Array.from(again.listKey)).toEqual(Array.from(made.listKey));
    expect(service.claims).toBe(3);
  });

  it("a creation the network cut finishes at the next call with the same list key", async () => {
    const { service, store } = setup();
    const id = lineage();
    // The keys lock lands (claim, head), then the list's claim is lost.
    let puts = 0;
    service.failNext((r) => r.method === "PUT" && ++puts > 2, 1);
    await expect(store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id })).rejects.toBeTruthy();
    expect(service.heads()).toBe(1);
    const keys = await store.readKeysLock(id);
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    expect(Array.from(made.listKey)).toEqual(Array.from(keys?.listKey ?? []));
    expect(service.heads()).toBe(3);
  });

  it("without the server half and without a keys lock nothing is made", async () => {
    const { service, store } = setup();
    expect(await store.ensure({ exportKey: EXPORT_KEY, serverHalf: null, lineageIdentity: lineage() })).toBe("absent");
    expect(service.vaults.size).toBe(0);
    expect(await store.openWithKeys(lineage())).toBe("absent");
    expect(await store.deleteAll(lineage())).toBe(0);
  });

  it("every list seals to one size: the service learns no count", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    const listVault = vaultIdOf(made.listKey);
    const empty = service.vaults.get(listVault)?.body?.length;
    await store.writeList(made.listKey, add(entry(1, [1, 2, 3, 4], bytes(32, 9)), entry(2, [7])));
    expect(service.vaults.get(listVault)?.body?.length).toBe(empty);
    // The version prefix, then one sealed 64 KiB bucket.
    expect(wasm.ownedChannelsListLen()).toBe(64 * 1024 - 4);
    expect(BigInt(empty ?? 0)).toBe(8n + (wasm.vaultSealedLen(BigInt(wasm.ownedChannelsListLen())) ?? 0n));
  });
});

describe("opening with each lock (items 2, 3, 3a)", () => {
  it("the keys lock gives R and S; the password and passkey locks give R without reading the keys lock", async () => {
    const { service, store } = setup();
    const id = lineage();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    await store.writeList(made.listKey, add(entry(5, [3])));
    await store.addPasskeyLock({ listKey: made.listKey, serverHalf: S, credentialTag: TAG, prfOutput: PRF });

    const byKeys = await store.openWithKeys(id);
    if (byKeys === "absent") throw new Error("the keys lock did not open");
    expect(Array.from(byKeys.listKey)).toEqual(Array.from(made.listKey));
    expect(Array.from(byKeys.serverHalf ?? [])).toEqual(Array.from(S));
    expect(byKeys.list.channels.map((c) => c.idHex)).toEqual([hexOf(bytes(16, 5))]);

    const keysLockPath = `/vault/${vaultIdOf(wasm.ownedKeysLockRoot(id))}/head`;
    for (const open of [
      () => store.openWithPassword({ exportKey: EXPORT_KEY, serverHalf: S }),
      () => store.openWithPasskey({ prfOutput: PRF, serverHalf: S }),
    ]) {
      service.seen.length = 0;
      const opened = await open();
      if (opened === "absent") throw new Error("the lock did not open");
      expect(Array.from(opened.listKey)).toEqual(Array.from(made.listKey));
      expect(opened.serverHalf).toBeNull();
      expect(opened.list.channels.length).toBe(1);
      expect(service.seen.some((r) => r.path === keysLockPath)).toBe(false);
    }
  });

  it("the wrong password, the wrong server half, another passkey and another account's keys open nothing", async () => {
    const { store } = setup();
    await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    expect(await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S })).toBe("absent");
    expect(await store.openWithPassword({ exportKey: EXPORT_KEY, serverHalf: bytes(32, 0x23) })).toBe("absent");
    expect(await store.openWithPasskey({ prfOutput: PRF, serverHalf: S })).toBe("absent");
    expect(await store.openWithKeys(lineage(0xa2))).toBe("absent");
  });
});

describe("writing the list (item 6)", () => {
  it("a version conflict reads again, merges and writes again", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    const listVault = vaultIdOf(made.listKey);
    // Another device of the account writes channel B between this device's
    // read and its write, once.
    let raced = false;
    service.beforeHeadWrite = (id) => {
      if (id !== listVault || raced) return;
      raced = true;
      const theirs = add(entry(0xb0, [9]))(made.list);
      service.writeHeadDirect(listVault, (version) => sealList(made.listKey, version, theirs));
    };
    const written = await store.writeList(made.listKey, add(entry(0xa0, [4])));
    expect(written.written).toBe(true);
    expect(written.version).toBe(3n);
    expect(written.list.channels.map((c) => c.idHex)).toEqual([hexOf(bytes(16, 0xa0)), hexOf(bytes(16, 0xb0))]);
    const read = await store.readList(made.listKey);
    expect(read?.version).toBe(3n);
    expect(read?.list.channels.length).toBe(2);
  });

  it("gives up after a bounded number of lost writes", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    const listVault = vaultIdOf(made.listKey);
    let n = 0;
    service.beforeHeadWrite = (id) => {
      if (id !== listVault) return;
      n += 1;
      const theirs = add(entry(0xc0, [n]))(made.list);
      service.writeHeadDirect(listVault, (version) => sealList(made.listKey, version, theirs));
    };
    const failure = await store.writeList(made.listKey, add(entry(0xa0, [4]))).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(OwnedVaultError);
    expect((failure as OwnedVaultError).code).toBe("conflict");
    expect(n).toBe(LIST_WRITE_ATTEMPTS);
  });

  it("a write that changes nothing sends nothing", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    await store.writeList(made.listKey, add(entry(1, [1])));
    const puts = service.seen.filter((r) => r.method === "PUT").length;
    const again = await store.writeList(made.listKey, add(entry(1, [1])));
    expect(again.written).toBe(false);
    expect(service.seen.filter((r) => r.method === "PUT").length).toBe(puts);
  });
});

describe("passkey locks (item 6)", () => {
  it("a passkey that keeps the chats gets a lock the list names; removing it deletes the lock", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    await store.addPasskeyLock({ listKey: made.listKey, serverHalf: S, credentialTag: TAG, prfOutput: PRF });
    const root = wasm.ownedPasskeyLockRoot(PRF, S);
    expect(service.vaults.has(vaultIdOf(root))).toBe(true);
    const named = (await store.readList(made.listKey))?.list.passkeyLocks ?? [];
    expect(named.map((l) => l.tagHex)).toEqual([hexOf(TAG)]);
    expect(Array.from(named[0]?.root ?? [])).toEqual(Array.from(root));

    expect(await store.removePasskeyLock(made.listKey, TAG)).toBe(true);
    expect(service.vaults.has(vaultIdOf(root))).toBe(false);
    expect((await store.readList(made.listKey))?.list.passkeyLocks).toEqual([]);
    expect(await store.openWithPasskey({ prfOutput: PRF, serverHalf: S })).toBe("absent");
    // A passkey the list never named: nothing to do.
    expect(await store.removePasskeyLock(made.listKey, TAG)).toBe(false);
  });

  it("a ninth passkey lock is refused", async () => {
    const { store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    for (let i = 0; i < 8; i += 1) {
      await store.addPasskeyLock({
        listKey: made.listKey,
        serverHalf: S,
        credentialTag: bytes(16, i + 1),
        prfOutput: bytes(32, i + 1),
      });
    }
    const failure = await store
      .addPasskeyLock({ listKey: made.listKey, serverHalf: S, credentialTag: bytes(16, 99), prfOutput: bytes(32, 99) })
      .catch((e: unknown) => e);
    expect((failure as OwnedVaultError).code).toBe("full");
  });
});

describe("a password change (item 7)", () => {
  it("the lock is stale until made again; the old lock is deleted and the list names the new root", async () => {
    const { service, store } = setup();
    const id = lineage();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    const oldRoot = wasm.ownedPasswordLockRoot(EXPORT_KEY, S);
    const newRoot = wasm.ownedPasswordLockRoot(NEW_EXPORT_KEY, S);
    expect(await store.passwordLockIsStale(EXPORT_KEY, S)).toBe(false);
    expect(await store.passwordLockIsStale(NEW_EXPORT_KEY, S)).toBe(true);

    expect(await store.relockPassword({ listKey: made.listKey, serverHalf: S, newExportKey: NEW_EXPORT_KEY })).toBe("made");
    expect(await store.passwordLockIsStale(NEW_EXPORT_KEY, S)).toBe(false);
    expect(service.vaults.has(vaultIdOf(oldRoot))).toBe(false);
    expect(service.vaults.has(vaultIdOf(newRoot))).toBe(true);
    expect(Array.from((await store.readList(made.listKey))?.list.passwordLockRoot ?? [])).toEqual(Array.from(newRoot));
    expect(await store.openWithPassword({ exportKey: EXPORT_KEY, serverHalf: S })).toBe("absent");
    const opened = await store.openWithPassword({ exportKey: NEW_EXPORT_KEY, serverHalf: S });
    if (opened === "absent") throw new Error("the new password lock did not open");
    expect(Array.from(opened.listKey)).toEqual(Array.from(made.listKey));
    // Again: nothing to do.
    expect(await store.relockPassword({ listKey: made.listKey, serverHalf: S, newExportKey: NEW_EXPORT_KEY })).toBe("current");
    expect(service.heads()).toBe(3);
  });

  it("a device with the keys does it with the S of the keys lock, holding no S of its own", async () => {
    const { store } = setup();
    const id = lineage();
    await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    const ensured = await store.ensure({ exportKey: NEW_EXPORT_KEY, serverHalf: null, lineageIdentity: id });
    if (ensured === "absent") throw new Error("the keys lock did not open");
    expect(ensured.passwordLock).toBe("made");
    expect(await store.passwordLockIsStale(NEW_EXPORT_KEY, S)).toBe(false);
    const second = await store.ensure({ exportKey: NEW_EXPORT_KEY, serverHalf: null, lineageIdentity: id });
    if (second === "absent") throw new Error("the keys lock did not open");
    expect(second.passwordLock).toBe("current");
  });

  it("a re-lock cut after the old lock went is finished by the next call", async () => {
    const { service, store } = setup();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: lineage() });
    const listPath = `/vault/${vaultIdOf(made.listKey)}/head`;
    service.failNext((r) => r.method === "PUT" && r.path === listPath, 1);
    await expect(
      store.relockPassword({ listKey: made.listKey, serverHalf: S, newExportKey: NEW_EXPORT_KEY }),
    ).rejects.toBeTruthy();
    // The new lock is there, the old one gone, the list still names the old.
    expect(await store.passwordLockIsStale(NEW_EXPORT_KEY, S)).toBe(false);
    expect(await store.passwordLockIsStale(EXPORT_KEY, S)).toBe(true);
    expect(await store.relockPassword({ listKey: made.listKey, serverHalf: S, newExportKey: NEW_EXPORT_KEY })).toBe("made");
    expect(Array.from((await store.readList(made.listKey))?.list.passwordLockRoot ?? [])).toEqual(
      Array.from(wasm.ownedPasswordLockRoot(NEW_EXPORT_KEY, S)),
    );
  });
});

describe("account deletion (item 9)", () => {
  it("deletes the list and every lock, and leaves nothing", async () => {
    const { service, store } = setup();
    const id = lineage();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    await store.addPasskeyLock({ listKey: made.listKey, serverHalf: S, credentialTag: TAG, prfOutput: PRF });
    await store.writeList(made.listKey, add(entry(1, [1])));
    expect(service.vaults.size).toBe(4);
    expect(await store.deleteAll(id)).toBe(4);
    expect(service.vaults.size).toBe(0);
    expect(await store.deleteAll(id)).toBe(0);
  });

  it("a deletion the network cut is finished by the next call", async () => {
    const { service, store } = setup();
    const id = lineage();
    await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    let deletes = 0;
    service.failNext((r) => r.method === "DELETE" && ++deletes > 1, 1);
    await expect(store.deleteAll(id)).rejects.toBeTruthy();
    expect(service.vaults.size).toBeGreaterThan(0);
    await store.deleteAll(id);
    expect(service.vaults.size).toBe(0);
  });
});

describe("the recovery key (item 5)", () => {
  it("is the list key's: one per channel, the same on every device that opens the list", async () => {
    const { store } = setup();
    const id = lineage();
    const made = await store.createAll({ exportKey: EXPORT_KEY, serverHalf: S, lineageIdentity: id });
    const byPassword = await store.openWithPassword({ exportKey: EXPORT_KEY, serverHalf: S });
    if (byPassword === "absent") throw new Error("the password lock did not open");
    const channel = bytes(16, 7);
    const pk = (listKey: Uint8Array, channelId: Uint8Array): number[] => {
      const key = wasm.ChannelRecoveryKey.fromListKey(listKey, channelId);
      try {
        return Array.from(key.verifyingKey());
      } finally {
        key.free();
      }
    };
    expect(pk(byPassword.listKey, channel)).toEqual(pk(made.listKey, channel));
    expect(pk(made.listKey, bytes(16, 8))).not.toEqual(pk(made.listKey, channel));
    expect(pk(bytes(32, 1), channel)).not.toEqual(pk(made.listKey, channel));
    // The removed derivations are gone from the core.
    expect("fromOprfOutput" in wasm.ChannelRecoveryKey).toBe(false);
    expect("channelRecoveryOprfInput" in wasm).toBe(false);
  });
});
