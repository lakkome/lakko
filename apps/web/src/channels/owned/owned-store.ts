// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 (items 2 to 9) - the owned-channels list and its locks
 * on the storage service, on web.
 *
 * Four kinds of ADR-0064 vault, each named by a 32-byte root through
 * `vaultKeysFromRoot(root)` and `VaultRef.ownedChannels()`, each holding one
 * head:
 *
 * | Vault         | Root                                    | Head            |
 * |---------------|-----------------------------------------|-----------------|
 * | password lock | `ownedPasswordLockRoot(exportKey, S)`   | `R`             |
 * | keys lock     | `ownedKeysLockRoot(lineageIdentity)`    | `R ‖ S`         |
 * | passkey lock  | `ownedPasskeyLockRoot(prfOutput, S)`    | `R`             |
 * | list          | `R`                                     | the list        |
 *
 * `R` is the random list key, `S` the per-account half `services/auth`
 * releases at sign-up and against a reset factor grant. The list names the
 * current password lock root and every passkey lock root, so whoever holds
 * `R` can delete a stale lock.
 *
 * This class is the I/O over a {@link OwnedVaultPort} (the storage vault
 * client; the tests pass a fake service). It decides nothing about channels:
 * the rules are in `owned-list.ts`, the runtime in `owned-runtime.ts`.
 * Every method throws on a network failure or a refusal it does not name
 * (`VaultHttpError`), and {@link OwnedVaultError} on a head that does not
 * open; the callers in `index.ts` turn both into an outcome to retry.
 *
 * Order of writes, chosen so a crash leaves nothing unreachable:
 * - creation writes the keys lock FIRST (it holds `R` and `S`: a later call
 *   finds it and finishes the rest), then the list, then the password lock;
 * - a new password lock is written, the old one deleted, and only then does
 *   the list name the new root (a crash between leaves the list naming a
 *   lock that is gone, which the next call repairs; the other order would
 *   leave the old password able to open the list with nobody knowing its
 *   root any more).
 *
 * Nothing here logs a root, a key, a seed or a vault id.
 */
import type { Wasm } from "../../wasm";
import { VaultClient, type VaultSigner } from "../../storage/vault-client";
import {
  decodeOwnedList,
  emptyOwnedList,
  encodeOwnedList,
  hexOf,
  sameBytes,
  sameList,
  type OwnedList,
} from "./owned-list";

type Identity = InstanceType<Wasm["Identity"]>;

/** The vault routes this store uses; `VaultClient` in the app. */
export type OwnedVaultPort = Pick<VaultClient, "claim" | "getHead" | "putHead" | "deleteVault">;

/** A head that is there and does not open or decode, or a list write that
 *  kept losing its version. Carries a code only. */
export class OwnedVaultError extends Error {
  override name = "OwnedVaultError";
  constructor(public readonly code: "unreadable" | "list-absent" | "conflict" | "unclaimed" | "full") {
    super(`owned channels: ${code}`);
  }
}

/** What a lock opened to. `listKey` and `serverHalf` are secret. */
export interface OpenedOwned {
  /** The list key `R`. */
  readonly listKey: Uint8Array;
  /** The server half `S`: only the keys lock holds it. */
  readonly serverHalf: Uint8Array | null;
  readonly list: OwnedList;
  readonly listVersion: bigint;
}

/** How many times a list write reads, merges and writes again after a
 *  version conflict before it gives up (chosen). */
export const LIST_WRITE_ATTEMPTS = 5;

interface RootVault {
  readonly signer: VaultSigner;
  seal(version: bigint, plain: Uint8Array): Uint8Array;
  open(version: bigint, sealed: Uint8Array): Uint8Array;
  release(): void;
}

function rootVault(wasm: Wasm, root: Uint8Array): RootVault {
  const keys = wasm.vaultKeysFromRoot(root);
  const ref = wasm.VaultRef.ownedChannels();
  return {
    signer: {
      vaultId: () => keys.vaultId(ref),
      writePublicKey: () => keys.writePublicKey(ref),
      signRequest: (method, path, body) => keys.signRequest(ref, method, path, body),
    },
    seal: (version, plain) => keys.sealHead(ref, version, plain),
    open: (version, sealed) => keys.openHead(ref, version, sealed),
    release() {
      try {
        ref.free();
      } catch {
        /* already freed */
      }
      try {
        keys.free();
      } catch {
        /* already freed */
      }
    },
  };
}

/** The list and its locks, over one vault port. */
export class OwnedChannelsStore {
  constructor(
    private readonly wasm: Wasm,
    private readonly port: OwnedVaultPort = new VaultClient(),
  ) {}

  // --------------------------------------------------------------------------
  // One vault
  // --------------------------------------------------------------------------

  private async withVault<T>(root: Uint8Array, fn: (v: RootVault) => Promise<T>): Promise<T> {
    const v = rootVault(this.wasm, root);
    try {
      return await fn(v);
    } finally {
      v.release();
    }
  }

  /** The opened head of the vault at `root`, or `null` when there is none
   *  (nobody claimed it, or it holds no head). */
  private async readHead(root: Uint8Array): Promise<{ version: bigint; plain: Uint8Array } | null> {
    return this.withVault(root, async (v) => {
      const r = await this.port.getHead(v.signer);
      if (r.kind !== "head") return null;
      try {
        return { version: r.head.version, plain: v.open(r.head.version, r.head.sealed) };
      } catch {
        throw new OwnedVaultError("unreadable");
      }
    });
  }

  /**
   * Make the vault at `root` hold `plain` when it holds nothing yet: claimed
   * if nobody claimed it (one `vault-claim` token), then written at version
   * 1. `"exists"` when it already holds a head (which is left alone).
   */
  private async writeNew(root: Uint8Array, plain: Uint8Array): Promise<"written" | "exists"> {
    return this.withVault(root, async (v) => {
      const r = await this.port.getHead(v.signer);
      if (r.kind === "head") return "exists";
      if (r.kind === "unclaimed") await this.port.claim(v.signer);
      const answer = await this.port.putHead(v.signer, 1n, v.seal(1n, plain));
      if (answer === "written") return "written";
      if (answer === "conflict") return "exists";
      throw new OwnedVaultError("unclaimed");
    });
  }

  /** Delete the vault at `root` (signed with its write key). Whether a vault
   *  was there. */
  private async deleteAt(root: Uint8Array): Promise<"deleted" | "absent"> {
    return this.withVault(root, (v) => this.port.deleteVault(v.signer));
  }

  // --------------------------------------------------------------------------
  // The lock roots
  // --------------------------------------------------------------------------

  /** `lock_pw` of an export key and `S`. */
  passwordLockRoot(exportKey: Uint8Array, serverHalf: Uint8Array): Uint8Array {
    return this.wasm.ownedPasswordLockRoot(exportKey, serverHalf);
  }

  /** `lock_keys` of the lineage identity. */
  keysLockRoot(lineageIdentity: Identity): Uint8Array {
    return this.wasm.ownedKeysLockRoot(lineageIdentity);
  }

  /** `lock_pk` of a passkey's `prf` output and `S`. */
  passkeyLockRoot(prfOutput: Uint8Array, serverHalf: Uint8Array): Uint8Array {
    return this.wasm.ownedPasskeyLockRoot(prfOutput, serverHalf);
  }

  // --------------------------------------------------------------------------
  // Reading
  // --------------------------------------------------------------------------

  /** The list under `R`, or `null` when its vault holds none. */
  async readList(listKey: Uint8Array): Promise<{ list: OwnedList; version: bigint } | null> {
    const head = await this.readHead(listKey);
    if (head === null) return null;
    try {
      return { list: decodeOwnedList(this.wasm, head.plain), version: head.version };
    } catch {
      throw new OwnedVaultError("unreadable");
    } finally {
      head.plain.fill(0);
    }
  }

  private async openListKeyLock(root: Uint8Array): Promise<OpenedOwned | "absent"> {
    const head = await this.readHead(root);
    if (head === null) return "absent";
    let listKey: Uint8Array;
    try {
      listKey = this.wasm.decodeOwnedListKeyLock(head.plain);
    } catch {
      throw new OwnedVaultError("unreadable");
    } finally {
      head.plain.fill(0);
    }
    const read = await this.readList(listKey);
    if (read === null) return "absent";
    return { listKey, serverHalf: null, list: read.list, listVersion: read.version };
  }

  /** Item 3: a device with the account keys. `R`, `S` and the list, or
   *  `"absent"` when the keys lock or the list is not there. */
  async openWithKeys(lineageIdentity: Identity): Promise<OpenedOwned | "absent"> {
    const keys = await this.readKeysLock(lineageIdentity);
    if (keys === null) return "absent";
    const read = await this.readList(keys.listKey);
    if (read === null) return "absent";
    return { listKey: keys.listKey, serverHalf: keys.serverHalf, list: read.list, listVersion: read.version };
  }

  /** ADR-0101 section 7 item 8, a recall (ADR-0074): the keys lock of the
   *  account's OLD keys, named by its root (`ownedKeysLockRoot` of the old
   *  lineage identity, which the recall record keeps). */
  async openWithKeysRoot(root: Uint8Array): Promise<OpenedOwned | "absent"> {
    const keys = await this.readKeysLockAt(root);
    if (keys === null) return "absent";
    const read = await this.readList(keys.listKey);
    if (read === null) return "absent";
    return { listKey: keys.listKey, serverHalf: keys.serverHalf, list: read.list, listVersion: read.version };
  }

  /** The keys lock alone: `R` and `S`, or `null` when there is none. */
  async readKeysLock(lineageIdentity: Identity): Promise<{ listKey: Uint8Array; serverHalf: Uint8Array } | null> {
    return this.readKeysLockAt(this.keysLockRoot(lineageIdentity));
  }

  private async readKeysLockAt(root: Uint8Array): Promise<{ listKey: Uint8Array; serverHalf: Uint8Array } | null> {
    const head = await this.readHead(root);
    if (head === null) return null;
    try {
      const both = this.wasm.decodeOwnedKeysLock(head.plain);
      try {
        return { listKey: both.slice(0, 32), serverHalf: both.slice(32, 64) };
      } finally {
        both.fill(0);
      }
    } catch {
      throw new OwnedVaultError("unreadable");
    } finally {
      head.plain.fill(0);
    }
  }

  /** Item 2: the password and the server half. Never reads the keys lock. */
  async openWithPassword(input: {
    readonly exportKey: Uint8Array;
    readonly serverHalf: Uint8Array;
  }): Promise<OpenedOwned | "absent"> {
    return this.openListKeyLock(this.passwordLockRoot(input.exportKey, input.serverHalf));
  }

  /** Item 3a: a passkey that keeps the chats and the server half. Never
   *  reads the keys lock. */
  async openWithPasskey(input: {
    readonly prfOutput: Uint8Array;
    readonly serverHalf: Uint8Array;
  }): Promise<OpenedOwned | "absent"> {
    return this.openListKeyLock(this.passkeyLockRoot(input.prfOutput, input.serverHalf));
  }

  /** Item 7: "a device knows the lock is stale when the password lock its
   *  export key names does not exist". */
  async passwordLockIsStale(exportKey: Uint8Array, serverHalf: Uint8Array): Promise<boolean> {
    return (await this.readHead(this.passwordLockRoot(exportKey, serverHalf))) === null;
  }

  // --------------------------------------------------------------------------
  // Writing the list
  // --------------------------------------------------------------------------

  /**
   * Item 6: read the list, apply `mutate`, write it at the next version; on
   * `409 vault-version-conflict` read again, apply `mutate` to what was read
   * (the mutations of `owned-list.ts` merge this device's entries into it
   * with the core's merge) and write again, at most
   * {@link LIST_WRITE_ATTEMPTS} times. A `mutate` that changes nothing
   * writes nothing. Throws {@link OwnedVaultError} `list-absent` when there
   * is no list, `conflict` when every attempt lost, `full` when the result
   * does not fit the list's bounds.
   */
  async writeList(
    listKey: Uint8Array,
    mutate: (current: OwnedList) => OwnedList,
  ): Promise<{ list: OwnedList; version: bigint; written: boolean }> {
    for (let attempt = 0; attempt < LIST_WRITE_ATTEMPTS; attempt += 1) {
      const current = await this.readList(listKey);
      if (current === null) throw new OwnedVaultError("list-absent");
      const next = mutate(current.list);
      if (sameList(next, current.list)) return { list: current.list, version: current.version, written: false };
      let plain: Uint8Array;
      try {
        plain = encodeOwnedList(this.wasm, next);
      } catch {
        throw new OwnedVaultError("full");
      }
      const version = current.version + 1n;
      const answer = await this.withVault(listKey, (v) => {
        const sealed = v.seal(version, plain);
        plain.fill(0);
        return this.port.putHead(v.signer, version, sealed);
      });
      if (answer === "written") return { list: next, version, written: true };
      if (answer === "unclaimed") throw new OwnedVaultError("unclaimed");
      // "conflict": another device of the account wrote in between.
    }
    throw new OwnedVaultError("conflict");
  }

  // --------------------------------------------------------------------------
  // Making and re-making the locks
  // --------------------------------------------------------------------------

  /**
   * Make whatever is missing of the keys lock, the list and the password
   * lock, and bring the password lock in line with `exportKey` (item 7).
   *
   * - The keys lock is there: its `R` and `S` are used, whatever
   *   `serverHalf` says.
   * - It is not there: with `serverHalf` a new `R` is drawn and the keys
   *   lock written; without it nothing can be made (`"absent"`).
   *
   * Idempotent: a second call writes nothing. Returns what was opened or
   * made, and whether the password lock was made here.
   */
  async ensure(input: {
    readonly exportKey: Uint8Array;
    readonly serverHalf: Uint8Array | null;
    readonly lineageIdentity: Identity;
  }): Promise<(OpenedOwned & { readonly passwordLock: "current" | "made" }) | "absent"> {
    let keys = await this.readKeysLock(input.lineageIdentity);
    if (keys === null) {
      if (input.serverHalf === null) return "absent";
      const listKey = this.wasm.generateOwnedChannelsListKey();
      const made = await this.makeKeysLock(input.lineageIdentity, listKey, input.serverHalf);
      // Another tab of this device won the write: its list key is the one.
      keys = made === "exists" ? await this.readKeysLock(input.lineageIdentity) : { listKey, serverHalf: input.serverHalf.slice() };
      if (keys === null) throw new OwnedVaultError("unreadable");
    }
    const { listKey, serverHalf } = keys;
    const root = this.passwordLockRoot(input.exportKey, serverHalf);
    const plain = encodeOwnedList(this.wasm, emptyOwnedList(root));
    try {
      await this.writeNew(listKey, plain);
    } finally {
      plain.fill(0);
    }
    const passwordLock = await this.relockPassword({ listKey, serverHalf, newExportKey: input.exportKey });
    const read = await this.readList(listKey);
    if (read === null) throw new OwnedVaultError("list-absent");
    return { listKey, serverHalf, list: read.list, listVersion: read.version, passwordLock };
  }

  /**
   * Item 4, "the device that signs up makes all three at once": the keys
   * lock (`R ‖ S`), the list (root `R`) and the password lock (`R`).
   * Idempotent when they exist.
   */
  async createAll(input: {
    readonly exportKey: Uint8Array;
    readonly serverHalf: Uint8Array;
    readonly lineageIdentity: Identity;
  }): Promise<OpenedOwned> {
    const made = await this.ensure(input);
    if (made === "absent") throw new OwnedVaultError("unreadable");
    return made;
  }

  /** Write the keys lock of `lineageIdentity` holding `R ‖ S` (creation, and
   *  item 8: the new keys lock under the new lineage seed after a reset).
   *  `"exists"` when that lock already holds a head, which is left alone. */
  async makeKeysLock(
    lineageIdentity: Identity,
    listKey: Uint8Array,
    serverHalf: Uint8Array,
  ): Promise<"written" | "exists"> {
    const head = this.wasm.encodeOwnedKeysLock(listKey, serverHalf);
    try {
      return await this.writeNew(this.keysLockRoot(lineageIdentity), head);
    } finally {
      head.fill(0);
    }
  }

  /**
   * Item 7: make the password lock of `newExportKey` (root from it and `S`,
   * head `R`), delete the lock the list named before, and record the new
   * root in the list. `"current"` when the list already names this lock and
   * the lock is there (nothing written); `"made"` otherwise.
   */
  async relockPassword(input: {
    readonly listKey: Uint8Array;
    readonly serverHalf: Uint8Array;
    readonly newExportKey: Uint8Array;
  }): Promise<"current" | "made"> {
    const root = this.passwordLockRoot(input.newExportKey, input.serverHalf);
    const read = await this.readList(input.listKey);
    if (read === null) throw new OwnedVaultError("list-absent");
    const named = read.list.passwordLockRoot;
    const head = this.wasm.encodeOwnedListKeyLock(input.listKey);
    let wrote: "written" | "exists";
    try {
      wrote = await this.writeNew(root, head);
    } finally {
      head.fill(0);
    }
    if (sameBytes(named, root)) return wrote === "written" ? "made" : "current";
    await this.deleteAt(named);
    await this.writeList(input.listKey, (l) => ({ ...l, passwordLockRoot: root }));
    return "made";
  }

  /**
   * Item 6: a passkey that keeps the chats was added on a device with the
   * keys. Its lock (root from its `prf` output and `S`, head `R`) is made
   * and its root recorded in the list under the passkey's tag; a lock the
   * list named under that tag before is deleted. Throws
   * {@link OwnedVaultError} `full` past eight passkey locks.
   */
  async addPasskeyLock(input: {
    readonly listKey: Uint8Array;
    readonly serverHalf: Uint8Array;
    readonly credentialTag: Uint8Array;
    readonly prfOutput: Uint8Array;
  }): Promise<void> {
    const tagHex = hexOf(input.credentialTag);
    const root = this.passkeyLockRoot(input.prfOutput, input.serverHalf);
    const head = this.wasm.encodeOwnedListKeyLock(input.listKey);
    try {
      await this.writeNew(root, head);
    } finally {
      head.fill(0);
    }
    const before = await this.readList(input.listKey);
    if (before === null) throw new OwnedVaultError("list-absent");
    const old = before.list.passkeyLocks.find((l) => l.tagHex === tagHex);
    if (old !== undefined && !sameBytes(old.root, root)) await this.deleteAt(old.root);
    await this.writeList(input.listKey, (l) => ({
      ...l,
      passkeyLocks: [...l.passkeyLocks.filter((p) => p.tagHex !== tagHex), { tagHex, root }].sort((a, b) =>
        a.tagHex < b.tagHex ? -1 : a.tagHex > b.tagHex ? 1 : 0,
      ),
    }));
  }

  /** Item 6: a passkey was removed. Its lock vault is deleted and the list
   *  no longer names it. Whether the list named one. */
  async removePasskeyLock(listKey: Uint8Array, credentialTag: Uint8Array): Promise<boolean> {
    const tagHex = hexOf(credentialTag);
    const read = await this.readList(listKey);
    if (read === null) throw new OwnedVaultError("list-absent");
    const lock = read.list.passkeyLocks.find((l) => l.tagHex === tagHex);
    if (lock === undefined) return false;
    await this.deleteAt(lock.root);
    await this.writeList(listKey, (l) => ({ ...l, passkeyLocks: l.passkeyLocks.filter((p) => p.tagHex !== tagHex) }));
    return true;
  }

  /** Item 8: "deletes every passkey lock the list names (the reset deleted
   *  the passkeys)". How many were named. */
  async removeAllPasskeyLocks(listKey: Uint8Array): Promise<number> {
    const read = await this.readList(listKey);
    if (read === null) throw new OwnedVaultError("list-absent");
    for (const lock of read.list.passkeyLocks) await this.deleteAt(lock.root);
    if (read.list.passkeyLocks.length > 0) await this.writeList(listKey, (l) => ({ ...l, passkeyLocks: [] }));
    return read.list.passkeyLocks.length;
  }

  /**
   * Item 9: before the account is deleted, delete every passkey lock and
   * the password lock the list names, the list, and the keys lock of
   * `lineageIdentity`, each signed with its own vault's write key. Returns
   * how many vaults were deleted (0 when the account never had any).
   */
  async deleteAll(lineageIdentity: Identity): Promise<number> {
    const keys = await this.readKeysLock(lineageIdentity);
    if (keys === null) return 0;
    let gone = 0;
    try {
      // A list that does not open names no lock; a network failure throws
      // (the caller tries again, the keys lock still standing).
      const read = await this.readList(keys.listKey).catch((err: unknown) => {
        if (err instanceof OwnedVaultError && err.code === "unreadable") return null;
        throw err;
      });
      if (read !== null) {
        for (const lock of read.list.passkeyLocks) {
          if ((await this.deleteAt(lock.root)) === "deleted") gone += 1;
        }
        if ((await this.deleteAt(read.list.passwordLockRoot)) === "deleted") gone += 1;
      }
      if ((await this.deleteAt(keys.listKey)) === "deleted") gone += 1;
      // Last: while the keys lock stands, a retry of this call finds the rest.
      if ((await this.deleteAt(this.keysLockRoot(lineageIdentity))) === "deleted") gone += 1;
      return gone;
    } finally {
      keys.listKey.fill(0);
      keys.serverHalf.fill(0);
    }
  }
}
