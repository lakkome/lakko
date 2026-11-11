// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * DeviceGroup lifecycle on the web client
 * (Phase 2a3b2-bootstrap, ADR-0007).
 *
 * One entry point — [`ensureDeviceGroup`] — wraps the
 * "load existing, or found when this device may" decision plus the
 * snapshot persistence so the per-route call sites see a single function.
 *
 * Mental model:
 *
 * ```text
 *   ensureDeviceGroup(...) ──┬─ snapshot found and readable ──→ Identity.restoreStorage(snapshot)
 *                            │                                  then loadDeviceGroup(identity, accountId, lineage)
 *                            │                                  → live handle
 *                            │
 *                            └─ no usable snapshot ───────────→ (an unreadable one is deleted first)
 *                                 ├─ after a key reset, or the account's directory
 *                                 │  lists no other own device (`mayFound`) ────→ DeviceGroup.createFounder(...)
 *                                 │                                                claim (or reset) the journal
 *                                 │                                                putDeviceGroupSnapshot(...)
 *                                 └─ otherwise ───────────────────────────────→ DeviceGroupNotJoinedError:
 *                                                                                 the device REJOINS through a
 *                                                                                 sibling (`device-group-rejoin.ts`)
 * ```
 *
 * ADR-0081 decision 4: re-founding is reserved for an account with no other
 * device. Before it, a device with no usable snapshot founded a new
 * DeviceGroup and reset the journal, cutting off every live sibling. Since
 * the third amendment (2026-11-11) the rejoin itself founds (this function,
 * `mayFound` answering yes) when no listed device answers its knock in time:
 * a listed device may be gone for good.
 *
 * `lineage` is the account identity the DeviceGroup id is derived from
 * (ADR-0078 section 6, [`deviceGroupLineageSeed`]): the id is a function of
 * the account seed, not of the account id, so a device without the seed
 * cannot name its DeviceGroup.
 *
 * Distinct DeviceIdentity provider — the wasm `Identity` this
 * module builds from the cached `deviceIdentitySeed` has its
 * own in-memory OpenMLS provider, independent of the
 * AccountIdentity provider that `ChatProvider` holds for
 * conversation groups (ADR-0007). Snapshot/restore goes
 * through this DeviceIdentity provider's storage only.
 *
 * Every DeviceGroup snapshot write here FAILS CLOSED (ADR-0081 decision 3):
 * a failed write throws and the operation in progress stops.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { loadWasm } from "../wasm";
import { getAccountIdentitySeed } from "../auth/account-identity-seed-store";
import { conversationSeed, getKeyRotationState } from "../auth/key-rotation-store";
import { DsClient } from "../ds/client";
import { dsAccess, takeSigner } from "../ds/access";
import { claimOrReset, releaseAuthority } from "../ds/ownership";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import {
  deleteDeviceGroupSnapshot,
  getDeviceGroupSnapshotState,
  putDeviceGroupSnapshot,
} from "./device-group-snapshot-store";

/** Options of [`ensureDeviceGroup`]. */
export interface EnsureDeviceGroupOptions {
  /**
   * ADR-0073 section 5 - the founding follows a key reset. A cached snapshot
   * is NOT restored on this path: it is the old generation's DeviceGroup, and
   * the caller deleted it. Since ADR-0078 phase 6 nothing else differs: the
   * new seed derives a new DeviceGroup id, a fresh journal the founding claims
   * plainly. Requires `accountIdentitySeed` (the NEW seed).
   */
  readonly afterKeyReset?: boolean;
  /**
   * ADR-0081 decision 4 - asked only when this device holds no usable
   * DeviceGroup state: `true` when the account's directory lists no OTHER own
   * device, so founding cuts nobody off. A throw (the directory could not be
   * read) propagates and nothing is founded. Absent: this call never founds
   * and throws {@link DeviceGroupNotJoinedError} instead - every one-shot
   * caller (pairing, removal, the policy, the device list) passes nothing,
   * so only the sync runtime ever founds or rejoins.
   */
  readonly mayFound?: () => Promise<boolean>;
}

/**
 * This device holds no usable DeviceGroup state and may not found one: the
 * account has other devices (`reason: "rejoin"`), so it must be admitted
 * again by one of them, or the caller passed no founding policy
 * (`reason: "no-state"`).
 */
export class DeviceGroupNotJoinedError extends Error {
  override name = "DeviceGroupNotJoinedError";
  constructor(public readonly reason: "rejoin" | "no-state") {
    super(
      reason === "rejoin"
        ? "this device is not in its account's DeviceGroup and must be added again by another device"
        : "this device holds no DeviceGroup state",
    );
  }
}

/**
 * ADR-0078 section 6 - the 64-byte seed the account's DeviceGroup id is
 * derived from: the seed the conversation leaves are bound to, i.e. the
 * OLDEST own predecessor after a proven rotation (a rotation keeps the
 * DeviceGroup, so its id must not move), else the account seed. `explicit`
 * (the caller's current account seed) is used when no predecessor is held;
 * without it the device's stored account seed is read. `null` when the
 * device holds no account seed at all (a device still waiting for its
 * recovery phrase): it cannot name its DeviceGroup yet.
 */
export async function deviceGroupLineageSeed(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  explicit?: Uint8Array,
): Promise<Uint8Array | null> {
  try {
    const predecessor = conversationSeed(await getKeyRotationState(accountIdB64, deviceKey));
    if (predecessor !== null) return predecessor;
  } catch (err) {
    // An unreadable rotation row (a device key that changed, no IndexedDB)
    // reads as "never rotated": the account seed is used.
    console.warn("key-rotation state unreadable; the DeviceGroup id is derived from the account seed", err);
  }
  if (explicit !== undefined) return explicit;
  return getAccountIdentitySeed(accountIdB64, deviceKey);
}

/** The account's DeviceGroup journal id ([`deviceGroupLineageSeed`]), or
 *  `null` when the device cannot derive it yet. */
export async function deviceGroupIdOf(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  explicit?: Uint8Array,
): Promise<Uint8Array | null> {
  const seed = await deviceGroupLineageSeed(accountIdB64, deviceKey, explicit);
  if (seed === null) return null;
  const wasm = await loadWasm();
  const account = wasm.Identity.fromSeed(seed);
  try {
    return wasm.deviceGroupIdFor(account);
  } finally {
    account.free();
  }
}

/** Result of [`ensureDeviceGroup`]. */
export interface EnsuredDeviceGroup {
  /** The wasm `Identity` built from the DeviceIdentity seed,
   *  populated with the (loaded or freshly-bootstrapped) MLS
   *  provider state. Caller holds this for the duration of
   *  the session; closing the handle wipes the provider. */
  readonly identity: WasmIdentity;
  /** The live DeviceGroup handle. */
  readonly group: WasmDeviceGroup;
  /** `true` when this call bootstrapped a fresh DeviceGroup
   *  (this device is now the founder). `false` when the
   *  handle was restored from a cached snapshot. */
  readonly bootstrapped: boolean;
  /** The generation of the stored snapshot this handle was read from (or
   *  written as), for a conditional write later
   *  (`device-group-snapshot-store.ts`). */
  readonly generation: number;
}

/**
 * Resolve the local DeviceGroup state: restore the cached snapshot, or found
 * a fresh DeviceGroup when `options.mayFound` (or a key reset) allows it.
 *
 * @param accountIdBytes - Raw 16-byte UUIDv7.
 * @param accountIdB64 - Base64 (Foundation-style padded) form of `accountIdBytes`.
 * @param deviceIdentitySeed - The 32-byte random seed cached
 *   by `ensureDeviceIdentitySeed` in `device-identity-store.ts`.
 * @param deviceKey - Non-extractable AES-GCM CryptoKey from
 *   `deriveDeviceRootKey` — used to unwrap the snapshot
 *   envelope in IDB.
 * @throws {DeviceGroupNotJoinedError} when there is no usable state and this
 *   call may not found.
 */
export async function ensureDeviceGroup(
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceIdentitySeed: Uint8Array,
  deviceKey: DeviceCipher,
  /** ADR-0057 — the account's phrase seed, so a founding can claim (or, on a
   *  re-found, reset) the journal with the account's root even before the
   *  session has installed its signer factory. Optional: without it the
   *  registry's root is used. */
  accountIdentitySeed?: Uint8Array,
  options: EnsureDeviceGroupOptions = {},
): Promise<EnsuredDeviceGroup> {
  const wasm = await loadWasm();
  const afterKeyReset = options.afterKeyReset === true;
  if (afterKeyReset && accountIdentitySeed === undefined) {
    throw new Error("ds: a key-reset founding needs the account seed for the new root");
  }
  const lineageSeed = await deviceGroupLineageSeed(accountIdB64, deviceKey, accountIdentitySeed);
  if (lineageSeed === null) {
    throw new Error("ds: the DeviceGroup id needs the account seed, which this device does not hold yet");
  }
  const lineage = wasm.Identity.fromSeed(lineageSeed);
  try {
    return await ensureDeviceGroupWith(
      wasm,
      lineage,
      accountIdBytes,
      accountIdB64,
      deviceIdentitySeed,
      deviceKey,
      accountIdentitySeed,
      afterKeyReset,
      options.mayFound,
    );
  } finally {
    lineage.free();
  }
}

async function ensureDeviceGroupWith(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  lineage: WasmIdentity,
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceIdentitySeed: Uint8Array,
  deviceKey: DeviceCipher,
  accountIdentitySeed: Uint8Array | undefined,
  afterKeyReset: boolean,
  mayFound: (() => Promise<boolean>) | undefined,
): Promise<EnsuredDeviceGroup> {
  // Build the per-device Identity from the cached seed. This
  // is the DeviceIdentity (ADR-0007), NOT the AccountIdentity
  // ChatProvider holds. Distinct OpenMLS provider, distinct
  // signature keys.
  const identity = wasm.Identity.fromSeed(deviceIdentitySeed);
  try {
    if (!afterKeyReset) {
      const loaded = await loadDurable(wasm, identity, lineage, accountIdBytes, accountIdB64, deviceKey);
      if (loaded !== null) return { identity, group: loaded.group, bootstrapped: false, generation: loaded.generation };
    }
    // No usable state. ADR-0081 decision 4: found only when nobody can be cut
    // off - after a key reset (a fresh journal) or when the account's
    // directory lists no other own device.
    if (!afterKeyReset) {
      if (mayFound === undefined) throw new DeviceGroupNotJoinedError("no-state");
      if (!(await mayFound())) throw new DeviceGroupNotJoinedError("rejoin");
    }
    const founded = await found(wasm, identity, lineage, accountIdBytes, accountIdB64, deviceKey, accountIdentitySeed, afterKeyReset);
    return { identity, group: founded.group, bootstrapped: true, generation: founded.generation };
  } catch (err) {
    try {
      identity.free();
    } catch {
      /* already freed */
    }
    throw err;
  }
}

/** The durable DeviceGroup, or `null` when there is none this device can use
 *  - an unreadable snapshot (a device key that changed, tampering) or one
 *  without the DeviceGroup row is DELETED first (ADR-0081 decision 4, step
 *  1: the device drops its local DeviceGroup state), together with any
 *  pending commit built on it. */
async function loadDurable(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  identity: WasmIdentity,
  lineage: WasmIdentity,
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<{ group: WasmDeviceGroup; generation: number } | null> {
  let state: Awaited<ReturnType<typeof getDeviceGroupSnapshotState>> = null;
  let unusable = false;
  try {
    state = await getDeviceGroupSnapshotState(accountIdB64, deviceKey);
  } catch (err) {
    console.warn("DeviceGroup snapshot unreadable under this device key", err instanceof Error ? err.name : "");
    unusable = true;
  }
  if (state !== null && state.snapshot.length > 0) {
    try {
      identity.restoreStorage(state.snapshot);
      const loaded = wasm.loadDeviceGroup(identity, accountIdBytes, lineage);
      if (loaded !== undefined) return { group: loaded, generation: state.generation };
      unusable = true;
    } catch (err) {
      console.warn("DeviceGroup snapshot restore failed", err instanceof Error ? err.name : "");
      unusable = true;
    }
  }
  if (unusable) await deleteDeviceGroupSnapshot(accountIdB64);
  return null;
}

/** Found a DeviceGroup with this device as its only leaf and claim (or reset)
 *  its journal BEFORE the founding is persisted (fail-closed, the second
 *  security pass, finding 3). */
async function found(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  identity: WasmIdentity,
  lineage: WasmIdentity,
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceKey: DeviceCipher,
  accountIdentitySeed: Uint8Array | undefined,
  afterKeyReset: boolean,
): Promise<{ group: WasmDeviceGroup; generation: number }> {
  const group = wasm.DeviceGroup.createFounder(identity, accountIdBytes, lineage);
  try {
    // ADR-0057 obligations 1 + 7 — the account owns its DeviceGroup, so every
    // device holds its root. A 409 means the id already carries an older
    // DeviceGroup's chain under the same root: a single-device account that
    // lost its state, the one case a reset cuts nobody off (ADR-0081
    // decision 4), and the root then RESETS it.
    const founding = takeSigner(() => group.dsAccessSigner(identity));
    if (founding === null) throw new Error("ds: the DeviceGroup founding key could not be derived");
    const groupId = group.groupId();
    let authority = dsAccess.journalAuthority(groupId);
    if ((authority.kind !== "root" || afterKeyReset) && accountIdentitySeed !== undefined) {
      releaseAuthority(authority);
      const account = wasm.Identity.fromSeed(accountIdentitySeed);
      try {
        authority = {
          kind: "root",
          signer: wasm.dsJournalRootSigner(account, groupId),
          rootSeed: wasm.dsJournalRootSeed(account.ed25519Seed, groupId),
        };
      } finally {
        account.free();
      }
    }
    try {
      if (authority.kind !== "root") {
        throw new Error("ds: the DeviceGroup root is not available yet");
      }
      await claimOrReset(new DsClient(), groupId, authority.signer, founding.epoch, founding.verifyingKey());
    } finally {
      releaseAuthority(authority);
      founding.free();
    }
    // A founding drops whatever was pending for the old state.
    await deleteDeviceGroupSnapshot(accountIdB64);
    const generation = await putDeviceGroupSnapshot(accountIdB64, identity.snapshotStorage(), deviceKey);
    return { group, generation };
  } catch (err) {
    console.warn("ds: DeviceGroup founding failed; not founded", err instanceof Error ? err.name : "");
    try {
      group.free();
    } catch {
      /* already freed */
    }
    throw err;
  }
}

/**
 * Persist the DeviceIdentity provider snapshot after a DeviceGroup mutation
 * made through `identity` (the pairing joiner's join from a Welcome).
 *
 * FAIL-CLOSED (ADR-0081 decision 3): a failed write throws, so the caller
 * stops - a join or a commit is never treated as done when its state did not
 * reach the disk.
 */
export async function persistDeviceGroupSnapshot(
  identity: WasmIdentity,
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<number> {
  const snapshot = identity.snapshotStorage();
  return putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey);
}

/**
 * The receive loop's persist: the snapshot AND the journal position it has
 * consumed up to, in one write, fail-closed. `expectedGeneration` makes the
 * write conditional on the stored row still being the one this handle read
 * or last wrote: a commit of this device that made its next epoch current
 * through another handle replaced it, and this handle's state is then an
 * epoch behind. Resolves to the new generation.
 */
export async function persistDeviceGroupSnapshotWithCursor(
  identity: WasmIdentity,
  accountIdB64: string,
  deviceKey: DeviceCipher,
  journalCursor: number | undefined,
  expectedGeneration?: number,
): Promise<number> {
  const snapshot = identity.snapshotStorage();
  return putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey, journalCursor, expectedGeneration);
}
