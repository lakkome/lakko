// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * DeviceGroup lifecycle on the web client
 * (Phase 2a3b2-bootstrap, ADR-0007).
 *
 * One entry point — [`ensureDeviceGroup`] — wraps the
 * "load existing or create-founder" decision plus the
 * post-mutation snapshot persistence so the per-route call
 * sites see a single function.
 *
 * Mental model:
 *
 * ```text
 *   signup / login / recover
 *           │
 *           ▼
 *   ensureDeviceGroup(...) ──┬─ snapshot found in IDB ──→ Identity.restoreStorage(snapshot)
 *                            │                            then loadDeviceGroup(identity, accountId)
 *                            │                            → live handle (or null if storage carried
 *                            │                              an unrelated row)
 *                            │
 *                            └─ no snapshot ─────────────→ DeviceGroup.createFounder(identity, accountId)
 *                                                          then identity.snapshotStorage()
 *                                                          then putDeviceGroupSnapshot(...)
 *                                                          → live handle (this device is now the
 *                                                            founding leaf of the user's DeviceGroup)
 * ```
 *
 * Distinct DeviceIdentity provider — the wasm `Identity` this
 * module builds from the cached `deviceIdentitySeed` has its
 * own in-memory OpenMLS provider, independent of the
 * AccountIdentity provider that `ChatProvider` holds for
 * conversation groups (ADR-0007). Snapshot/restore goes
 * through this DeviceIdentity provider's storage only.
 */
import { loadWasm } from "../wasm";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import {
  getDeviceGroupSnapshot,
  putDeviceGroupSnapshot,
} from "./device-group-snapshot-store";

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
}

/**
 * Resolve the local DeviceGroup state: either restore the
 * cached snapshot or bootstrap a fresh founder leaf.
 *
 * Idempotent on the snapshot side — calling twice with the
 * same `(accountIdB64, deviceIdentitySeed, deviceKey)` triple
 * returns the same group (the bootstrap branch fires only
 * when the snapshot is absent).
 *
 * @param accountIdBytes - Raw 16-byte UUIDv7.
 * @param accountIdB64 - Base64 (Foundation-style padded) form of `accountIdBytes`.
 * @param deviceIdentitySeed - The 32-byte random seed cached
 *   by `ensureDeviceIdentitySeed` in `device-identity-store.ts`.
 * @param deviceKey - Non-extractable AES-GCM CryptoKey from
 *   `deriveDeviceRootKey` — used to unwrap the snapshot
 *   envelope in IDB.
 */
export async function ensureDeviceGroup(
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceIdentitySeed: Uint8Array,
  deviceKey: CryptoKey,
): Promise<EnsuredDeviceGroup> {
  const wasm = await loadWasm();

  // Build the per-device Identity from the cached seed. This
  // is the DeviceIdentity (ADR-0007), NOT the AccountIdentity
  // ChatProvider holds. Distinct OpenMLS provider, distinct
  // signature keys.
  const identity = wasm.Identity.fromSeed(deviceIdentitySeed);

  // Try to restore prior MLS state.
  const snapshot = await getDeviceGroupSnapshot(accountIdB64, deviceKey);
  if (snapshot !== null && snapshot.length > 0) {
    try {
      identity.restoreStorage(snapshot);
      const loaded = wasm.loadDeviceGroup(identity, accountIdBytes);
      if (loaded !== undefined) {
        return { identity, group: loaded, bootstrapped: false };
      }
      // Snapshot existed but didn't carry the DeviceGroup
      // row — fall through to bootstrap. Defensive: this
      // shouldn't happen in normal flows because every
      // snapshot we write was captured immediately after a
      // DeviceGroup mutation.
    } catch (err) {
      // Decrypt / restore failed (device key rotated since
      // the snapshot was written, ciphertext tampered, …).
      // Fall through to bootstrap; the recovered device key
      // produces a new founding leaf rather than the prior
      // one, which is the canonical "fresh-device-on-recovery"
      // behaviour.
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup snapshot restore failed, bootstrapping", err);
    }
  }

  // Bootstrap. Local device becomes the founding leaf of the
  // user's DeviceGroup.
  const group = wasm.DeviceGroup.createFounder(identity, accountIdBytes);
  const fresh = identity.snapshotStorage();
  await putDeviceGroupSnapshot(accountIdB64, fresh, deviceKey);
  return { identity, group, bootstrapped: true };
}

/**
 * Re-capture the MLS provider snapshot after a DeviceGroup
 * mutation (add-device, remove-device, application-message
 * send). Call site pattern:
 *
 * ```ts
 *   const { identity, group } = await ensureDeviceGroup(...);
 *   group.addDeviceFromWire(identity, kp);   // mutates state
 *   await persistDeviceGroupSnapshot(identity, accountIdB64, deviceKey);
 * ```
 *
 * Failures log + swallow — a snapshot persist failure
 * shouldn't tear down the in-memory state.
 */
export async function persistDeviceGroupSnapshot(
  identity: WasmIdentity,
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<void> {
  try {
    const snapshot = identity.snapshotStorage();
    await putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("DeviceGroup snapshot persist failed", err);
  }
}
