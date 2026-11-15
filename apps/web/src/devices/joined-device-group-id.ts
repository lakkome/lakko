// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-56 (ADR-0074 "Joining after a rotation") — the id of the DeviceGroup this
 * device joined by pairing, per account, sealed under the device key.
 *
 * A device that joined the account's DeviceGroup by pairing and then parked
 * for the account's earlier keys cannot derive the DeviceGroup id: the id is
 * a function of the account's earliest key (`lifecycle.ts`, the lineage
 * guard), which it does not hold. Pairing again from the parked screen has to
 * name that group in its self-removal proof and in its epoch probe, so the id
 * is recorded at every join: the pairing join of a known account
 * (`prepareJoinFlow`) and the join by name (`prepareAnonymousJoinFlow`, at
 * `persistTo`).
 *
 * Kept in the key-rotation row (`auth/key-rotation-store.ts`), whose lifetime
 * is the one needed: Forget device deletes the row, and so do a key reset and
 * a recall on this device.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getKeyRotationState, updateKeyRotationState } from "../auth/key-rotation-store";

/** Record the joined DeviceGroup's id. Never throws: a failure is logged and
 *  only costs the re-pairing from the parked screen its proof (it falls back
 *  to the ordinary join). */
export async function recordJoinedDeviceGroupId(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  groupId: Uint8Array,
): Promise<void> {
  if (groupId.length !== 32) return;
  const b64 = base64Encode(groupId);
  try {
    await updateKeyRotationState(accountIdB64, deviceKey, (state) =>
      state.joinedDeviceGroupIdB64 === b64 ? state : { ...state, joinedDeviceGroupIdB64: b64 },
    );
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("the joined DeviceGroup id was not recorded", err instanceof Error ? err.name : "unknown");
  }
}

/** The recorded id, or `null` when none was recorded or the row does not
 *  open under `deviceKey`. Never throws. */
export async function readJoinedDeviceGroupId(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  try {
    const b64 = (await getKeyRotationState(accountIdB64, deviceKey)).joinedDeviceGroupIdB64;
    return b64 === undefined ? null : base64Decode(b64);
  } catch {
    return null;
  }
}
