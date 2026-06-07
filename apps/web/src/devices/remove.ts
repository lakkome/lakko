// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Remove-device flow on the web client (Phase 2a3c).
 *
 * Mirror of the iOS / Android remove-device helpers. Three
 * call sites the UI exercises:
 *
 *   * [`listDevices`] — enumerate every device leaf in the
 *     user's DeviceGroup + tell the UI which row is "this
 *     device" so the corresponding Remove button can be
 *     disabled (the user removes a non-self device; for the
 *     self case the flow is `forgetDevice` on THIS browser,
 *     not Remove on another browser).
 *
 *   * [`removeDeviceAt`] — build the MLS Remove commit for the
 *     given leaf, broadcast it to every remaining device via
 *     the DS group journal so they advance past the revoked
 *     leaf and ratchet group secrets out of the removed
 *     device's reach, persist the snapshot.
 *
 * The DeviceGroup journal is the existing
 * `POST/GET /ds/group/{group_id_hex}/messages` endpoint —
 * `group_id` is the deterministic
 * `BLAKE3("lakko-device-group/v1" || account_id)` value the
 * server already accepts as opaque bytes.
 */
import { ensureDeviceGroup, persistDeviceGroupSnapshot } from "./lifecycle";
import { encodeJournalEntry } from "./journal-envelope";
import { DsClient } from "../ds/client";

/** One row of the device list. Shape matches the wasm
 *  `GroupMember` getter pair plus a `selfLeaf` boolean the UI
 *  uses to render the "(this device)" badge / disable the
 *  Remove button. */
export interface DeviceListItem {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  readonly isSelf: boolean;
}

/** Result of [`listDevices`]. */
export interface DeviceList {
  readonly ownLeafIndex: number;
  readonly devices: readonly DeviceListItem[];
}

/**
 * Snapshot the current DeviceGroup membership for rendering
 * the management screen.
 *
 * Builds a fresh DeviceIdentity + ensures the local
 * DeviceGroup, then drains the FFI handles before returning.
 * The list is a snapshot — the UI calls this again after every
 * mutation (add or remove) to pick up the new epoch's leaf
 * numbering.
 */
export async function listDevices(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
}): Promise<DeviceList> {
  const { ensureDeviceIdentitySeed } = await import("../auth/device-identity-store");
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  const ensured = await ensureDeviceGroup(
    args.accountIdBytes,
    args.accountIdB64,
    seed,
    args.deviceKey,
  );
  seed.fill(0);
  try {
    const ownLeafIndex = ensured.group.ownLeafIndex();
    const rows = ensured.group.devices() as unknown[];
    const devices: DeviceListItem[] = rows.map((row) => {
      // `wasm.devices()` returns Vec<JsValue> where each value
      // is a wasm-bindgen GroupMember instance — the JS shape
      // is `{ leafIndex, signaturePublicKey }` via the getter
      // pair declared on the Rust side.
      const m = row as { leafIndex: number; signaturePublicKey: Uint8Array };
      return {
        leafIndex: m.leafIndex,
        signaturePublicKey: m.signaturePublicKey,
        isSelf: m.leafIndex === ownLeafIndex,
      };
    });
    return { ownLeafIndex, devices };
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
}

/** Result of [`removeDeviceAt`]. */
export interface RemoveResult {
  /** Number of devices in the local DeviceGroup AFTER the
   *  remove. UI surfaces as "now N devices linked". */
  readonly remainingCount: number;
}

/**
 * Revoke `leafIndex` from the DeviceGroup.
 *
 * Steps:
 *
 *   1. Ensure the local DeviceGroup (load existing snapshot or
 *      bootstrap a founder if absent — the latter shouldn't
 *      happen on the remove path; a snapshot must already
 *      exist for the local device to know about other leaves).
 *   2. Build the Remove commit. The local view advances
 *      atomically inside the wasm `removeDeviceAt` call.
 *   3. Broadcast the commit to the DeviceGroup DS journal so
 *      every other still-paired device advances to the new
 *      epoch on its next poll.
 *   4. Persist the resulting MLS provider snapshot to IDB so
 *      subsequent launches don't re-broadcast the same commit
 *      or attempt to remove an already-gone leaf.
 *
 * Errors are surfaced verbatim:
 *
 *   * `"…cannot remove own leaf…"` — caller passed
 *     `leafIndex === ownLeafIndex()`. UI guards against this,
 *     but the Rust side also rejects defensively.
 *   * `"…no such device leaf …"` — stale leaf index (the user
 *     stared at the list while another tab committed a
 *     remove). UI prompts for a refresh + retry.
 *   * DS `publishGroup` errors propagate as `DsHttpError` /
 *     `DsNetworkError`.
 */
export async function removeDeviceAt(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
  readonly leafIndex: number;
  /** G-13 (ADR-0015): the removal attestation bytes from
   *  `/auth/action/finish` (decoded from `attestation_b64`). Carried in the
   *  journal envelope so peers can verify the removal when the DeviceGroup
   *  policy requires it. `null` when the account has no passkey (policy off).
   */
  readonly attestation?: Uint8Array | null;
}): Promise<RemoveResult> {
  const { ensureDeviceIdentitySeed } = await import("../auth/device-identity-store");
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  const ensured = await ensureDeviceGroup(
    args.accountIdBytes,
    args.accountIdB64,
    seed,
    args.deviceKey,
  );
  seed.fill(0);

  try {
    const commitBytes = ensured.group.removeDeviceAt(
      ensured.identity,
      args.leafIndex,
    );
    // Broadcast to the DeviceGroup journal. The wasm `groupId`
    // accessor returns the deterministic 32-byte
    // BLAKE3-domain-tagged value; the DS treats it as opaque
    // bytes. G-13 (ADR-0015): wrap the commit + removal attestation in the
    // journal envelope so peers can verify the removal under the policy.
    const groupId = ensured.group.groupId();
    const ds = new DsClient();
    await ds.publishGroup(groupId, encodeJournalEntry(commitBytes, args.attestation ?? null));

    await persistDeviceGroupSnapshot(
      ensured.identity,
      args.accountIdB64,
      args.deviceKey,
    );
    return {
      remainingCount: ensured.group.devices().length,
    };
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
}
