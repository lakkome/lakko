// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The DeviceGroup filter (ADR-0074 section 2.3, "Own-account admission").
 *
 * A device of THIS account is added to a conversation only when its leaf key
 * is in the account's current DeviceGroup roster. The DeviceGroup is the
 * account's own, MLS-authenticated record of its devices, and a removed
 * device is not in it; the directory alone is not enough, because a removed
 * device that still holds the account seed could publish a row for itself
 * with the server's help. So every path that adds an own device asks here
 * first: the ADR-0050 re-admission arm, the pairing fan-out, the re-derived
 * admission after a superseded proposal, and the "include my other devices"
 * step of creating a conversation.
 *
 * The filter sits in front of a proposal the core would accept; it changes
 * nothing for a contact's devices (for those the test is still "listed, and
 * bound", and the per-chat removed memory of `chat/device-readmission.ts`
 * stays as the defence in depth it was).
 *
 * The roster is published by the DeviceGroup sync runtime
 * (`DeviceGroupSyncContext`), which is the only holder of the live handle.
 * It is module state, keyed by account, because the callers are plain
 * classes and modules outside the Solid tree.
 */

import { registerAccountMemory } from "../auth/account-memory";

/** The roster as the filter reads it: the account it belongs to and its
 *  devices' leaf signature keys, lowercase hex. */
export interface OwnDeviceRoster {
  readonly accountIdB64: string;
  readonly keysHex: ReadonlySet<string>;
}

let current: OwnDeviceRoster | null = null;

// The roster decides which of the account's own devices are admitted. One
// held from before Forget device names the device set of the DeviceGroup this
// device left; it must not admit anything in the account's next session on
// this page before the new roster is published.
registerAccountMemory(
  "own-device-roster",
  (ids) => {
    if (current !== null && ids.includes(current.accountIdB64)) current = null;
  },
  { signOut: true },
);

/** Publish the roster of `accountIdB64`, or `null` when no handle is loaded
 *  (signed out, an account switch, this device removed). */
export function publishOwnDeviceRoster(roster: { accountIdB64: string; keysHex: readonly string[] } | null): void {
  current =
    roster === null
      ? null
      : { accountIdB64: roster.accountIdB64, keysHex: new Set(roster.keysHex.map((k) => k.toLowerCase())) };
}

/** The roster of `accountIdB64`, or `null` when it is not known right now. */
export function ownDeviceRoster(accountIdB64: string): OwnDeviceRoster | null {
  return current !== null && current.accountIdB64 === accountIdB64 ? current : null;
}

export function hexOfKey(key: Uint8Array): string {
  let out = "";
  for (const byte of key) out += byte.toString(16).padStart(2, "0");
  return out;
}

/**
 * Whether an own-account device may be added to a conversation.
 *
 * Fails closed: with no roster (the DeviceGroup handle is not loaded) no own
 * device is added, and the scheduled re-admission arm adds it later, once
 * the roster is known and names it.
 */
export function ownDeviceAdmissible(roster: OwnDeviceRoster | null, deviceKeyHex: string): boolean {
  return roster !== null && roster.keysHex.has(deviceKeyHex.toLowerCase());
}
