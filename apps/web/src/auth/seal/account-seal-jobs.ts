// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a device that holds the account's keys does about the account's
 * published protection (ADR-0101 section 10) and its owned-channels locks
 * (section 7), and when. `AuthContext` reports events and calls
 * {@link SealJobs.settle} after every landing; everything here is behind
 * injected ports so the rules are tested without a network or the wasm of
 * the channels layer.
 *
 * | Event on this device                                   | Announcement                         | Owned channels                               |
 * |--------------------------------------------------------|--------------------------------------|----------------------------------------------|
 * | sign-up (export key and `S` in hand)                   | first one, password entry            | list, keys lock, password lock made          |
 * | a guest's first password (`S` kept since its sign-up)  | first one                            | the same                                     |
 * | a password sign-in, or a password change, here         | password entry replaced if different | password lock made again when stale          |
 * | a passkey that gave a seal secret was added            | its entry added                      | its lock made                                |
 * | a passkey was deleted                                  | its entry removed (owed until done)  | its lock deleted                             |
 * | the three-word check of "Save a recovery phrase"       | phrase entry added                   | -                                            |
 * | a key rotation made here                               | carried to the new key, new `seq`    | - (the keys lock follows the lineage seed)   |
 * | a key reset made here                                  | password entry alone, generation `g` | the retake, then new locks                   |
 * | any other landing                                      | read only                            | -                                            |
 *
 * The password entry is only ever made from an export key a password
 * registration or sign-in produced ON THIS PAGE: an export key cached on the
 * device may be the previous password's, and announcing it would put an old
 * password back.
 *
 * Secrets (export keys, `prf` outputs, `S`) are held in memory for as long
 * as the event that brought them is unsettled, and zeroed when it settles or
 * on {@link SealJobs.drop}. What an owed step still needs after a reload or
 * a crash is also kept under the account's device key (`S` in
 * `server-half-store.ts`, the rest in `owed-secrets-store.ts`) and deleted
 * when the step is done: a lock write that failed (a passkey's lock, the
 * password lock, a deleted passkey's lock) is tried again at every landing
 * until it succeeds, never dropped.
 */
import type { Wasm } from "../../wasm";
import type { Identity as WasmIdentity } from "../../../wasm/lakko_ffi_wasm";
import type { DeviceCipher } from "../device-cipher";
import type { MarkerStorage } from "../phrase-gate";
import { clearOwedSecrets, readOwedSecrets, writeOwedSecrets, type OwedSecrets } from "./owed-secrets-store";
import { hex, sealSecretListed, type SealChanges } from "./seal-announcement";
import { publishSeals, readOwnSeals, type PublishOutcome, type SealPorts } from "./seal-runtime";
import { clearServerHalf, keepServerHalf, keptServerHalf } from "./server-half-store";

/** The signed-in account and the keys this device holds for it. */
export interface SealAccount {
  readonly accountIdB64: string;
  readonly accountId: Uint8Array;
  /** The 64-byte seed of the account identity of the CURRENT key. */
  readonly accountIdentitySeed: Uint8Array;
  /** The 64-byte seed the account's conversations are bound to (the oldest
   *  own predecessor after a rotation, else the current seed). */
  readonly lineageSeed: Uint8Array;
  readonly keyGeneration: bigint;
}

/** What sealed a reset made on this device, for the owned-channels retake. */
export type ResetSealedBy =
  | { readonly kind: "password"; readonly exportKey: Uint8Array }
  | { readonly kind: "passkey"; readonly prfOutput: Uint8Array; readonly credentialTag: Uint8Array };

/** The owned-channels ports (`channels/owned`), as this module calls them. */
export interface OwnedPorts {
  readonly make: (input: {
    accountIdB64: string;
    exportKey: Uint8Array;
    serverHalf: Uint8Array;
    lineageIdentity: WasmIdentity;
  }) => Promise<"made" | "retry">;
  readonly ensurePasswordLock: (input: {
    accountIdB64: string;
    exportKey: Uint8Array;
    lineageIdentity: WasmIdentity;
  }) => Promise<"current" | "made" | "absent" | "retry">;
  readonly addPasskeyLock: (input: {
    accountIdB64: string;
    lineageIdentity: WasmIdentity;
    credentialTag: Uint8Array;
    prfOutput: Uint8Array;
  }) => Promise<"added" | "absent" | "full" | "retry">;
  readonly removePasskeyLock: (input: {
    accountIdB64: string;
    lineageIdentity: WasmIdentity;
    credentialTag: Uint8Array;
  }) => Promise<"removed" | "absent" | "retry">;
  readonly retake: (input: {
    accountIdB64: string;
    serverHalf: Uint8Array | null;
    sealedBy: ResetSealedBy | null;
    newOwner: WasmIdentity;
    newLineageIdentity: WasmIdentity;
    currentExportKey: Uint8Array | null;
    oldKeysRoot?: Uint8Array | null;
  }) => Promise<"done" | "retry" | "nothing">;
}

export interface SealJobDeps {
  readonly wasm: () => Promise<Wasm>;
  readonly ports: SealPorts;
  readonly owned: OwnedPorts;
  /** The signed-in account with its keys; `null` when no session with keys
   *  is on screen (signed out, parked without keys, a suspended account). */
  readonly account: () => SealAccount | null;
  readonly deviceKey: (accountIdB64: string) => Promise<DeviceCipher | null>;
  /** The recovery phrase this device holds for the account, or `null`. */
  readonly devicePhrase: (accountIdB64: string) => Promise<string | null>;
  /** The account keys this device holds from BEFORE the current one (the
   *  predecessors of an ADR-0074 rotation), newest first; empty when the
   *  account never rotated. An announcement this device kept is carried to
   *  the new key only when it opens under one of them. */
  readonly previousAccountKeys: (accountIdB64: string) => Promise<readonly Uint8Array[]>;
  /** The export key this device caches for the account (possibly a previous
   *  password's): used ONLY to open the password lock after a reset this
   *  device made and a reload lost the memory of. */
  readonly cachedExportKey: (accountIdB64: string) => Uint8Array | null;
  readonly storage: MarkerStorage | undefined;
}

export interface SealJobs {
  /** A password registration or sign-in just ran on this device. */
  notePassword(accountIdB64: string, exportKey: Uint8Array): void;
  /** `/signup/finish` (or the reset's request) released the server half. */
  noteServerHalf(accountIdB64: string, serverHalf: Uint8Array): Promise<void>;
  /** A key reset was made here: what sealed it (or `null`), and the export
   *  key of the password in force when this page proved it. A recall
   *  (ADR-0074) is one too: no seal, the new password's export key, and the
   *  root of the OLD keys lock (`oldKeysRoot`), which opens the
   *  owned-channels list the recall device takes back. */
  noteReset(input: {
    accountIdB64: string;
    sealedBy: ResetSealedBy | null;
    exportKey: Uint8Array | null;
    oldKeysRoot?: Uint8Array | null;
  }): void;
  /** Run whatever is owed for the account on screen. Serialised; never throws. */
  settle(): Promise<void>;
  savePhrase(): Promise<"published" | "retry">;
  passkeyAdded(input: { credentialId: Uint8Array; sealPrfOutput: Uint8Array | null }): Promise<boolean>;
  passkeyRemoved(credentialId: Uint8Array): Promise<void>;
  /** A rotation made here landed; `previousKey` is the account key before it. */
  afterRotation(previousKey: Uint8Array): Promise<PublishOutcome | "no-account">;
  /** The new generation's announcement after a reset's publish, for an
   *  account that is not on screen as a session yet. */
  announceAfterReset(account: SealAccount): Promise<PublishOutcome>;
  /** Forget every secret held in memory (sign-out, another account). */
  drop(): void;
}

function removalsKey(accountIdB64: string): string {
  return `lakko:seal-removals:${accountIdB64}`;
}

function retakeKey(accountIdB64: string): string {
  return `lakko:owned-retake-owed:${accountIdB64}`;
}

/** The tags (lower-case hex) of deleted passkeys whose seal entry is still
 *  to leave the announcement. */
export function owedSealRemovals(storage: MarkerStorage | undefined, accountIdB64: string): string[] {
  try {
    const raw = storage?.getItem(removalsKey(accountIdB64)) ?? null;
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((t): t is string => typeof t === "string" && /^[0-9a-f]{32}$/.test(t))
      : [];
  } catch {
    return [];
  }
}

function writeRemovals(storage: MarkerStorage | undefined, accountIdB64: string, tags: readonly string[]): void {
  try {
    if (tags.length === 0) storage?.removeItem(removalsKey(accountIdB64));
    else storage?.setItem(removalsKey(accountIdB64), JSON.stringify([...new Set(tags)]));
  } catch {
    // Storage blocked: the removal is retried only within this page.
  }
}

/** Forget the per-account rows this module keeps (Forget device). */
export function clearSealJobFacts(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(removalsKey(accountIdB64));
    storage?.removeItem(retakeKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
  clearServerHalf(storage, accountIdB64);
  clearOwedSecrets(storage, accountIdB64);
}

const landed = (outcome: PublishOutcome): boolean => outcome === "published" || outcome === "unchanged";

export function createSealJobs(deps: SealJobDeps): SealJobs {
  let password: { accountIdB64: string; exportKey: Uint8Array } | null = null;
  let serverHalf: { accountIdB64: string; bytes: Uint8Array } | null = null;
  let reset: {
    accountIdB64: string;
    sealedBy: ResetSealedBy | null;
    exportKey: Uint8Array | null;
    oldKeysRoot: Uint8Array | null;
    announced: boolean;
  } | null = null;
  let chain: Promise<unknown> = Promise.resolve();

  const serial = <T>(run: () => Promise<T>): Promise<T> => {
    const next = chain.then(run, run);
    chain = next.catch(() => undefined);
    return next;
  };

  function zero(bytes: Uint8Array | null | undefined): void {
    bytes?.fill(0);
  }

  function dropReset(): void {
    if (reset === null) return;
    zero(reset.exportKey);
    if (reset.sealedBy?.kind === "password") zero(reset.sealedBy.exportKey);
    if (reset.sealedBy?.kind === "passkey") zero(reset.sealedBy.prfOutput);
    zero(reset.oldKeysRoot);
    reset = null;
  }

  async function identities(wasm: Wasm, account: SealAccount) {
    return {
      own: wasm.Identity.fromSeed(account.accountIdentitySeed),
      lineage: wasm.Identity.fromSeed(account.lineageSeed),
    };
  }

  function ownAccount(account: SealAccount, own: WasmIdentity) {
    return {
      accountIdB64: account.accountIdB64,
      accountId: account.accountId,
      identity: own,
      keyGeneration: account.keyGeneration,
    };
  }

  async function heldServerHalf(account: SealAccount): Promise<Uint8Array | null> {
    const key = await deps.deviceKey(account.accountIdB64).catch(() => null);
    if (serverHalf !== null && serverHalf.accountIdB64 === account.accountIdB64) {
      // It may have arrived before the device key existed (the first steps
      // of a sign-up): it is written down now, so a reload does not lose it.
      if (key !== null) await keepServerHalf(deps.storage, account.accountIdB64, serverHalf.bytes, key);
      return serverHalf.bytes;
    }
    if (key === null) return null;
    const kept = await keptServerHalf(deps.storage, account.accountIdB64, key);
    if (kept !== null) serverHalf = { accountIdB64: account.accountIdB64, bytes: kept };
    return kept;
  }

  function serverHalfDone(accountIdB64: string): void {
    if (serverHalf !== null && serverHalf.accountIdB64 === accountIdB64) {
      zero(serverHalf.bytes);
      serverHalf = null;
    }
    clearServerHalf(deps.storage, accountIdB64);
  }

  function retakeOwed(accountIdB64: string): boolean {
    try {
      return deps.storage?.getItem(retakeKey(accountIdB64)) === "1";
    } catch {
      return false;
    }
  }

  function setRetakeOwed(accountIdB64: string, owed: boolean): void {
    try {
      if (owed) deps.storage?.setItem(retakeKey(accountIdB64), "1");
      else deps.storage?.removeItem(retakeKey(accountIdB64));
    } catch {
      // Storage blocked: the retake is owed only within this page.
    }
  }

  /** What an owed step still needs, kept under the device key. In memory
   *  only when the device key is not there (yet). */
  let owedInMemory: { accountIdB64: string; owed: OwedSecrets } | null = null;

  async function readOwed(accountIdB64: string): Promise<OwedSecrets> {
    const key = await deps.deviceKey(accountIdB64).catch(() => null);
    if (key !== null) {
      const stored = await readOwedSecrets(deps.storage, accountIdB64, key);
      if (owedInMemory === null || owedInMemory.accountIdB64 !== accountIdB64) return stored;
      // Written down now that the key exists.
      const merged = owedInMemory.owed;
      owedInMemory = null;
      const both: OwedSecrets = {
        reset: merged.reset ?? stored.reset,
        passkeyLocks: [...stored.passkeyLocks, ...merged.passkeyLocks],
        passkeyLockRemovals: [...new Set([...stored.passkeyLockRemovals, ...merged.passkeyLockRemovals])],
        passwordLock: merged.passwordLock ?? stored.passwordLock,
      };
      await writeOwedSecrets(deps.storage, accountIdB64, key, both);
      return both;
    }
    return owedInMemory !== null && owedInMemory.accountIdB64 === accountIdB64
      ? owedInMemory.owed
      : { reset: null, passkeyLocks: [], passkeyLockRemovals: [], passwordLock: null };
  }

  async function updateOwed(accountIdB64: string, change: (o: OwedSecrets) => OwedSecrets): Promise<void> {
    const next = change(await readOwed(accountIdB64));
    const key = await deps.deviceKey(accountIdB64).catch(() => null);
    if (key !== null && (await writeOwedSecrets(deps.storage, accountIdB64, key, next))) {
      if (owedInMemory?.accountIdB64 === accountIdB64) owedInMemory = null;
      return;
    }
    owedInMemory = { accountIdB64, owed: next };
  }

  const sameBytes = (a: Uint8Array, b: Uint8Array): boolean =>
    a.length === b.length && a.every((x, i) => x === b[i]);

  /** The owned channels after a reset made on this device (ADR-0101 section
   *  7 item 8). `true` when nothing more is owed. */
  async function settleRetake(account: SealAccount, own: WasmIdentity, lineage: WasmIdentity): Promise<boolean> {
    const inMemory = reset !== null && reset.accountIdB64 === account.accountIdB64 ? reset : null;
    const kept = inMemory === null ? (await readOwed(account.accountIdB64)).reset : null;
    if (inMemory === null && kept === null && !retakeOwed(account.accountIdB64)) return true;
    const half = await heldServerHalf(account);
    // After a reload: what sealed the reset, kept under the device key since
    // the reset; failing that (a row from before this build) the cached
    // export key, which opens the password lock when the password sealed it.
    const cached = inMemory === null && kept === null ? deps.cachedExportKey(account.accountIdB64) : null;
    const sealedBy: ResetSealedBy | null =
      inMemory !== null
        ? inMemory.sealedBy
        : kept !== null
          ? kept.sealedBy
          : cached !== null && cached.length === 64
            ? { kind: "password", exportKey: cached }
            : null;
    const currentExportKey =
      inMemory !== null
        ? inMemory.exportKey
        : kept !== null
          ? kept.exportKey
          : cached !== null && cached.length === 64
            ? cached
            : null;
    const oldKeysRoot = inMemory !== null ? inMemory.oldKeysRoot : (kept?.oldKeysRoot ?? null);
    const outcome = await deps.owned.retake({
      accountIdB64: account.accountIdB64,
      serverHalf: half,
      sealedBy,
      newOwner: own,
      newLineageIdentity: lineage,
      currentExportKey,
      oldKeysRoot,
    });
    if (outcome === "retry") return false;
    if (outcome === "nothing" && half !== null && currentExportKey !== null) {
      // No lock opened (an unsealed reset, or an account that had no list):
      // the new account gets a list of its own.
      const made = await deps.owned.make({
        accountIdB64: account.accountIdB64,
        exportKey: currentExportKey,
        serverHalf: half,
        lineageIdentity: lineage,
      });
      if (made === "retry") return false;
    }
    setRetakeOwed(account.accountIdB64, false);
    serverHalfDone(account.accountIdB64);
    await updateOwed(account.accountIdB64, (o) => ({ ...o, reset: null }));
    return true;
  }

  /**
   * Owned-channels lock writes that failed before (item C of the alignment
   * of 2026-11-27): a passkey's seal and lock, a deleted passkey's lock, the
   * password lock of a password event of this device. Each is tried again;
   * what lands leaves the owed row.
   */
  async function settleOwedLocks(
    wasm: Wasm,
    account: SealAccount,
    own: WasmIdentity,
    lineage: WasmIdentity,
    passwordEventHeld: boolean,
  ): Promise<void> {
    const owed = await readOwed(account.accountIdB64);
    const own_ = ownAccount(account, own);
    const doneLocks: Uint8Array[] = [];
    for (const p of owed.passkeyLocks) {
      if (!p.sealed) {
        const outcome = await publishSeals(wasm, deps.ports, own_, {
          addPasskey: { credentialId: p.credentialId, prfOutput: p.prfOutput },
        });
        if (outcome === "retry") continue;
        if (!landed(outcome)) {
          // The announcement cannot list it (eight passkeys, no password):
          // nothing to lock either.
          doneLocks.push(p.credentialId);
          continue;
        }
      }
      const added = await deps.owned.addPasskeyLock({
        accountIdB64: account.accountIdB64,
        lineageIdentity: lineage,
        credentialTag: wasm.resetSealPasskeyTag(p.credentialId),
        prfOutput: p.prfOutput,
      });
      if (added !== "retry") doneLocks.push(p.credentialId);
      else await updateOwed(account.accountIdB64, (o) => ({
        ...o,
        passkeyLocks: o.passkeyLocks.map((x) => (sameBytes(x.credentialId, p.credentialId) ? { ...x, sealed: true } : x)),
      }));
    }
    const doneRemovals: string[] = [];
    for (const tagHex of owed.passkeyLockRemovals) {
      const tag = Uint8Array.from(tagHex.match(/../g) ?? [], (h) => Number.parseInt(h, 16));
      const removed = await deps.owned.removePasskeyLock({
        accountIdB64: account.accountIdB64,
        lineageIdentity: lineage,
        credentialTag: tag,
      });
      if (removed !== "retry") doneRemovals.push(tagHex);
    }
    let passwordDone = false;
    if (owed.passwordLock !== null && !passwordEventHeld) {
      // Used again only while the account's announcement lists this
      // password: one changed elsewhere since must not get its lock back.
      const seals = await readOwnSeals(wasm, deps.ports, own_);
      if (seals !== null) {
        if (!sealSecretListed(wasm, seals, { kind: "password", exportKey: owed.passwordLock })) {
          passwordDone = true;
        } else {
          passwordDone = await writePasswordLock(account, lineage, owed.passwordLock);
        }
      }
    }
    if (doneLocks.length > 0 || doneRemovals.length > 0 || passwordDone) {
      await updateOwed(account.accountIdB64, (o) => ({
        ...o,
        passkeyLocks: o.passkeyLocks.filter((x) => !doneLocks.some((d) => sameBytes(d, x.credentialId))),
        passkeyLockRemovals: o.passkeyLockRemovals.filter((t) => !doneRemovals.includes(t)),
        passwordLock: passwordDone ? null : o.passwordLock,
      }));
    }
  }

  /** The password lock of `exportKey`: made with the list when `S` is held,
   *  else made again when stale. `true` when nothing more is owed. */
  async function writePasswordLock(account: SealAccount, lineage: WasmIdentity, exportKey: Uint8Array): Promise<boolean> {
    const half = await heldServerHalf(account);
    if (half !== null) {
      const made = await deps.owned.make({
        accountIdB64: account.accountIdB64,
        exportKey,
        serverHalf: half,
        lineageIdentity: lineage,
      });
      if (made === "made") serverHalfDone(account.accountIdB64);
      return made === "made";
    }
    const ensured = await deps.owned.ensurePasswordLock({
      accountIdB64: account.accountIdB64,
      exportKey,
      lineageIdentity: lineage,
    });
    return ensured !== "retry";
  }

  /**
   * The directory serves no announcement under the account's current key:
   * when this device kept one that opens under a key the account had before
   * (a rotation), it is carried to the current key and generation, the
   * phrase entry re-made from the phrase the device holds now. Whichever
   * device of the account lands first after a rotation does this.
   */
  async function carryAfterRotation(wasm: Wasm, account: SealAccount, own: WasmIdentity): Promise<void> {
    const previous = await deps.previousAccountKeys(account.accountIdB64).catch(() => []);
    if (previous.length === 0) return;
    const phrase = await deps.devicePhrase(account.accountIdB64).catch(() => null);
    for (const keptUnderKey of previous) {
      const outcome = await publishSeals(
        wasm,
        deps.ports,
        ownAccount(account, own),
        phrase !== null ? { rekeyPhrase: phrase } : {},
        { carryFromKept: { keptUnderKey } },
      );
      if (outcome !== "no-password") return;
    }
  }

  async function settleNow(): Promise<void> {
    const account = deps.account();
    if (account === null) return;
    const wasm = await deps.wasm();
    const { own, lineage } = await identities(wasm, account);
    try {
      const own_ = ownAccount(account, own);

      // 1. A reset made here: the new generation's announcement, then the
      // owned channels.
      const afterReset = reset !== null && reset.accountIdB64 === account.accountIdB64 ? reset : null;
      if (afterReset !== null && !afterReset.announced) {
        const outcome = await publishSeals(wasm, deps.ports, own_, {
          afterReset: true,
          ...(afterReset.exportKey !== null ? { passwordExportKey: afterReset.exportKey } : {}),
        });
        if (landed(outcome) || outcome === "no-password") afterReset.announced = true;
      }
      const retakeSettled = await settleRetake(account, own, lineage);
      if (retakeSettled && afterReset !== null && afterReset.announced) dropReset();

      // 2. Seals of passkeys deleted while the directory could not be told.
      const removals = owedSealRemovals(deps.storage, account.accountIdB64);
      let left = removals;
      for (const tagHex of removals) {
        const outcome = await publishSeals(wasm, deps.ports, own_, { removePasskeyTagHex: tagHex });
        if (!landed(outcome) && outcome !== "no-password") break;
        left = left.filter((t) => t !== tagHex);
      }
      if (left.length !== removals.length) writeRemovals(deps.storage, account.accountIdB64, left);

      // 3. Lock writes owed from before (a reload, an earlier failure).
      const event = password !== null && password.accountIdB64 === account.accountIdB64 ? password : null;
      if (retakeSettled) await settleOwedLocks(wasm, account, own, lineage, event !== null);

      // 4. A password event of this page.
      if (event === null) {
        if (afterReset === null && (await readOwnSeals(wasm, deps.ports, own_)) === null) {
          await carryAfterRotation(wasm, account, own);
        }
        return;
      }
      const announced = afterReset !== null
        ? true
        : landed(await publishSeals(wasm, deps.ports, own_, { passwordExportKey: event.exportKey }));
      const lockSettled = retakeSettled ? await writePasswordLock(account, lineage, event.exportKey) : false;
      // A lock that did not land is owed beyond this page; one that did
      // settles an older owed one.
      const lockKey = event.exportKey.slice();
      await updateOwed(account.accountIdB64, (o) => ({ ...o, passwordLock: lockSettled ? null : lockKey }));
      if (announced && lockSettled && password === event) {
        zero(event.exportKey);
        password = null;
      }
    } finally {
      own.free();
      lineage.free();
    }
  }

  async function withAccount<T>(
    fallback: T,
    run: (wasm: Wasm, account: SealAccount, own: WasmIdentity, lineage: WasmIdentity) => Promise<T>,
  ): Promise<T> {
    const account = deps.account();
    if (account === null) return fallback;
    try {
      const wasm = await deps.wasm();
      const { own, lineage } = await identities(wasm, account);
      try {
        return await run(wasm, account, own, lineage);
      } finally {
        own.free();
        lineage.free();
      }
    } catch {
      return fallback;
    }
  }

  return {
    notePassword(accountIdB64, exportKey) {
      if (exportKey.length !== 64) return;
      if (password !== null) zero(password.exportKey);
      password = { accountIdB64, exportKey: exportKey.slice() };
    },

    async noteServerHalf(accountIdB64, bytes) {
      if (bytes.length !== 32) return;
      if (serverHalf !== null) zero(serverHalf.bytes);
      serverHalf = { accountIdB64, bytes: bytes.slice() };
      const key = await deps.deviceKey(accountIdB64).catch(() => null);
      if (key !== null) await keepServerHalf(deps.storage, accountIdB64, bytes, key);
    },

    noteReset(input) {
      dropReset();
      reset = {
        accountIdB64: input.accountIdB64,
        sealedBy:
          input.sealedBy === null
            ? null
            : input.sealedBy.kind === "password"
              ? { kind: "password", exportKey: input.sealedBy.exportKey.slice() }
              : {
                  kind: "passkey",
                  prfOutput: input.sealedBy.prfOutput.slice(),
                  credentialTag: input.sealedBy.credentialTag.slice(),
                },
        exportKey: input.exportKey === null ? null : input.exportKey.slice(),
        oldKeysRoot: input.oldKeysRoot === undefined || input.oldKeysRoot === null ? null : input.oldKeysRoot.slice(),
        announced: false,
      };
      setRetakeOwed(input.accountIdB64, true);
      // What sealed it survives a reload until the retake answered.
      const keep = reset;
      void serial(() =>
        updateOwed(input.accountIdB64, (o) => ({
          ...o,
          reset: {
            sealedBy:
              keep.sealedBy === null
                ? null
                : keep.sealedBy.kind === "password"
                  ? { kind: "password", exportKey: keep.sealedBy.exportKey.slice() }
                  : {
                      kind: "passkey",
                      prfOutput: keep.sealedBy.prfOutput.slice(),
                      credentialTag: keep.sealedBy.credentialTag.slice(),
                    },
            exportKey: keep.exportKey?.slice() ?? null,
            oldKeysRoot: keep.oldKeysRoot?.slice() ?? null,
          },
        })).catch(() => undefined),
      );
      // The reset's own announcement replaces whatever password event the
      // page held for the account.
      if (password !== null && password.accountIdB64 === input.accountIdB64) {
        zero(password.exportKey);
        password = null;
      }
    },

    settle() {
      return serial(async () => {
        try {
          await settleNow();
        } catch {
          // Never into the caller: everything owed stays owed.
        }
      });
    },

    savePhrase() {
      return serial(() =>
        withAccount<"published" | "retry">("retry", async (wasm, account, own) => {
          const phrase = await deps.devicePhrase(account.accountIdB64);
          if (phrase === null) return "retry";
          const event = password !== null && password.accountIdB64 === account.accountIdB64 ? password : null;
          const changes: SealChanges = {
            savePhrase: phrase,
            ...(event !== null ? { passwordExportKey: event.exportKey } : {}),
          };
          return landed(await publishSeals(wasm, deps.ports, ownAccount(account, own), changes))
            ? "published"
            : "retry";
        }),
      );
    },

    passkeyAdded(input) {
      return serial(() =>
        withAccount(false, async (wasm, account, own, lineage) => {
          if (input.sealPrfOutput === null || input.sealPrfOutput.length !== 32) return false;
          const prf = input.sealPrfOutput;
          const outcome = await publishSeals(wasm, deps.ports, ownAccount(account, own), {
            addPasskey: { credentialId: input.credentialId, prfOutput: prf },
          });
          const owe = (sealed: boolean): Promise<void> =>
            updateOwed(account.accountIdB64, (o) => ({
              ...o,
              passkeyLocks: [
                ...o.passkeyLocks.filter((x) => !sameBytes(x.credentialId, input.credentialId)),
                { credentialId: input.credentialId.slice(), prfOutput: prf.slice(), sealed },
              ],
            }));
          if (outcome === "retry") {
            // Its seal and lock are written at a later landing.
            await owe(false);
            return false;
          }
          if (!landed(outcome)) return false;
          // Its lock of the owned-channels list; one that fails is owed.
          const added = await deps.owned.addPasskeyLock({
            accountIdB64: account.accountIdB64,
            lineageIdentity: lineage,
            credentialTag: wasm.resetSealPasskeyTag(input.credentialId),
            prfOutput: prf,
          });
          if (added === "retry") await owe(true);
          return true;
        }),
      );
    },

    passkeyRemoved(credentialId) {
      return serial(() =>
        withAccount<void>(undefined, async (wasm, account, own, lineage) => {
          const tag = wasm.resetSealPasskeyTag(credentialId);
          const tagHex = hex(tag);
          const outcome = await publishSeals(wasm, deps.ports, ownAccount(account, own), {
            removePasskeyTagHex: tagHex,
          });
          if (!landed(outcome) && outcome !== "no-password") {
            // The deleted passkey's seal must not stay announced: owed.
            writeRemovals(deps.storage, account.accountIdB64, [
              ...owedSealRemovals(deps.storage, account.accountIdB64),
              tagHex,
            ]);
          }
          const removed = await deps.owned.removePasskeyLock({
            accountIdB64: account.accountIdB64,
            lineageIdentity: lineage,
            credentialTag: tag,
          });
          await updateOwed(account.accountIdB64, (o) => ({
            ...o,
            // An add still owed for it is moot.
            passkeyLocks: o.passkeyLocks.filter((x) => !sameBytes(x.credentialId, credentialId)),
            passkeyLockRemovals:
              removed === "retry" ? [...new Set([...o.passkeyLockRemovals, tagHex])] : o.passkeyLockRemovals,
          }));
        }),
      );
    },

    afterRotation(previousKey) {
      return serial(() =>
        withAccount<PublishOutcome | "no-account">("no-account", async (wasm, account, own) => {
          const phrase = await deps.devicePhrase(account.accountIdB64);
          return publishSeals(
            wasm,
            deps.ports,
            ownAccount(account, own),
            phrase !== null ? { rekeyPhrase: phrase } : {},
            { carryFromKept: { keptUnderKey: previousKey } },
          );
        }),
      );
    },

    announceAfterReset(account) {
      return serial(async (): Promise<PublishOutcome> => {
        const held = reset !== null && reset.accountIdB64 === account.accountIdB64 ? reset : null;
        try {
          const wasm = await deps.wasm();
          const own = wasm.Identity.fromSeed(account.accountIdentitySeed);
          try {
            const outcome = await publishSeals(wasm, deps.ports, ownAccount(account, own), {
              afterReset: true,
              ...(held !== null && held.exportKey !== null ? { passwordExportKey: held.exportKey } : {}),
            });
            if (held !== null && (landed(outcome) || outcome === "no-password")) held.announced = true;
            return outcome;
          } finally {
            own.free();
          }
        } catch {
          return "retry";
        }
      });
    },

    drop() {
      if (password !== null) zero(password.exportKey);
      password = null;
      if (serverHalf !== null) zero(serverHalf.bytes);
      serverHalf = null;
      owedInMemory = null;
      dropReset();
    },
  };
}
