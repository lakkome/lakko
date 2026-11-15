// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 "Joining after a rotation" (contract G-56 section 1.3): what this
 * device last read of the account's OWN conversation link, per account,
 * sealed under the device key.
 *
 * The record rides in the key-rotation row (`key-rotation-store.ts`), which
 * is exactly the lifetime it needs: Forget device deletes the row, and so do
 * a key reset and a recall on this device (a reset keeps no predecessor and
 * clears the link). Written by the binding gate whenever it read a bundle,
 * and by the landing-time check of the own bundle; read by the gate (so a
 * warm landing costs no fetch) and by the DeviceGroup lineage guard
 * (`devices/lifecycle.ts`).
 */
import type { DeviceCipher } from "./device-cipher";
import { base64Decode, base64Encode } from "./account-id";
import { getKeyRotationState, updateKeyRotationState } from "./key-rotation-store";
import { OWN_LINK_ABSENT, type OwnLink } from "../devices/own-conversation-key";

/** The recorded answer as a link (`absent` or `key`), or `null` when this
 *  device never read its own bundle's link. Throws when the row does not
 *  open under `deviceKey`. */
export async function readOwnLinkRecord(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<OwnLink | null> {
  const record = (await getKeyRotationState(accountIdB64, deviceKey)).ownLink;
  if (record === undefined) return null;
  if (record.conversationKeyB64 === null) return OWN_LINK_ABSENT;
  return { kind: "key", key: base64Decode(record.conversationKeyB64) };
}

/** Record a link answer read from the own bundle. `unknown` (the bundle
 *  could not be read) records nothing. Never throws. */
export async function writeOwnLinkRecord(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  link: OwnLink,
  nowMs: number = Date.now(),
): Promise<void> {
  if (link.kind === "unknown") return;
  const conversationKeyB64 = link.kind === "key" ? base64Encode(link.key) : null;
  try {
    await updateKeyRotationState(accountIdB64, deviceKey, (state) =>
      state.ownLink?.conversationKeyB64 === conversationKeyB64 &&
      state.ownLink.readAtMs === nowMs
        ? state
        : { ...state, ownLink: { conversationKeyB64, readAtMs: nowMs } },
    );
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("own conversation link not recorded", err instanceof Error ? err.name : "unknown");
  }
}
