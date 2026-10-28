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
 * `BLAKE3-derive-key("lakko/device-group/id/v2", account_seed)` value
 * (ADR-0078 section 6) the server accepts as opaque bytes.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { ensureDeviceGroup } from "./lifecycle";
import { encodeJournalEntry } from "./journal-envelope";
import { commitDeviceGroupChange } from "./device-group-commit-runtime";

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
  readonly deviceKey: DeviceCipher;
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
 *   1. Load the durable DeviceGroup on a throwaway handle (never founds).
 *   2. Build the Remove commit on it.
 *   3. Run it through the ADR-0081 outbox (`device-group-commit.ts`): the
 *      entry beside the durable snapshot, the commit route, the next epoch
 *      made current and the sync runtime reloaded.
 *
 * Errors are surfaced verbatim:
 *
 *   * `"…cannot remove own leaf…"` — caller passed
 *     `leafIndex === ownLeafIndex()`. UI guards against this,
 *     but the Rust side also rejects defensively.
 *   * `"…no such device leaf …"` — stale leaf index (the user
 *     stared at the list while another tab committed a
 *     remove). UI prompts for a refresh + retry.
 *   * DS errors propagate as `DsHttpError` / `DsNetworkError` (the
 *     outbox entry stays and is finished later);
 *     {@link DeviceGroupCommitLostError} when another device's commit took
 *     the epoch.
 */
export async function removeDeviceAt(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly leafIndex: number;
  /** G-13 (ADR-0015): the removal attestation bytes from
   *  `/auth/action/finish` (decoded from `attestation_b64`). Carried in the
   *  journal envelope so peers can verify the removal when the DeviceGroup
   *  policy requires it. `null` when the account has no passkey (policy off).
   */
  readonly attestation?: Uint8Array | null;
  /** ADR-0081 decision 4 - the removal of a rejoining device's previous
   *  leaf rather than a removal the person asked for (diagnostics only). */
  readonly rejoin?: boolean;
}): Promise<RemoveResult> {
  // ADR-0081 decision 2 — through the DeviceGroup commit outbox: the entry
  // beside the durable snapshot, one `POST .../commit` (the next epoch's
  // registration and the commit row together, the row wrapping the removal
  // attestation for the ADR-0015 policy), the next epoch made current and
  // the sync runtime reloaded, the entry deleted. A lost epoch (another
  // device changed the DeviceGroup first) throws: the person looks again.
  const result = await commitDeviceGroupChange(
    {
      accountIdBytes: args.accountIdBytes,
      accountIdB64: args.accountIdB64,
      deviceKey: args.deviceKey,
    },
    (ensured) => {
      const commitBytes = ensured.group.removeDeviceAt(ensured.identity, args.leafIndex);
      return {
        kind: args.rejoin === true ? "rejoin-remove" : "remove",
        row: encodeJournalEntry(commitBytes, args.attestation ?? null),
      };
    },
    async (ensured, outcome) => {
      if (outcome.kind === "lost") throw new DeviceGroupCommitLostError();
      return ensured.group.devices().length;
    },
  );
  if (result === null) throw new DeviceGroupCommitLostError();
  return { remainingCount: result.value };
}

/** A DeviceGroup commit lost its epoch to another device's commit
 *  (ADR-0081): nothing of it is on the journal; the caller re-reads and
 *  tries again. */
export class DeviceGroupCommitLostError extends Error {
  override name = "DeviceGroupCommitLostError";
  constructor() {
    super("another device changed the account's devices at the same time");
  }
}

/** What {@link removeDeviceByKey} found. */
export type RemoveByKeyResult =
  /** The Remove commit was published. */
  | "removed"
  /** No leaf of the DeviceGroup holds that key (it was already removed). */
  | "absent"
  /** The DeviceGroup's policy requires a passkey attestation for a removal
   *  (ADR-0015) and none was given: nothing was sent. */
  | "needs-attestation";

/**
 * Revoke the device whose leaf signature key is `deviceKey` from the
 * DeviceGroup (ADR-0074 section 2.3, step 4 of the removal operation).
 *
 * Addressed by key, not by leaf index, so a retry from the owed record after
 * a reload finds the right leaf whatever the epoch's numbering is, and a
 * device another device already removed reads as done rather than as an
 * error. Never removes this device's own leaf.
 */
export async function removeDeviceByKey(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly targetKey: Uint8Array;
  readonly attestation: Uint8Array | null;
}): Promise<RemoveByKeyResult> {
  const { ensureDeviceIdentitySeed } = await import("../auth/device-identity-store");
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  let leafIndex: number | null = null;
  let policy = false;
  const ensured = await ensureDeviceGroup(args.accountIdBytes, args.accountIdB64, seed, args.deviceKey);
  seed.fill(0);
  try {
    const own = ensured.group.ownLeafIndex();
    for (const row of ensured.group.devices() as unknown[]) {
      const m = row as { leafIndex: number; signaturePublicKey: Uint8Array };
      if (m.leafIndex !== own && sameBytes(m.signaturePublicKey, args.targetKey)) {
        leafIndex = m.leafIndex;
      }
    }
    try {
      policy = ensured.group.requireAttestedRemovals();
    } catch {
      policy = false;
    }
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
  if (leafIndex === null) return "absent";
  if (policy && args.attestation === null) return "needs-attestation";
  await removeDeviceAt({
    accountIdBytes: args.accountIdBytes,
    accountIdB64: args.accountIdB64,
    deviceKey: args.deviceKey,
    leafIndex,
    attestation: args.attestation,
  });
  return "removed";
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}
