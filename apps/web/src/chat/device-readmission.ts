// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.4 / 2.6 — the re-admission arm, and its mirror.
 *
 * A person's device that has published a KeyPackage but holds no leaf in a
 * chat the person is in should be added: a device restored by three words
 * with no other device online, a device whose pairing fan-out was cut short,
 * a contact's new phone. Any member may propose it (the core's second
 * self-service arm), so every client re-checks, on a bounded schedule, the
 * device list of each account in each chat against that chat's roster.
 *
 * The mirror: a leaf of THIS account whose device no longer has a directory
 * row (`forgetDevice` deletes its row before wiping) is retired, by this
 * account's own devices only — a plain member may not remove another
 * account's leaf.
 *
 * What the planner cannot know is why a device is absent from a roster: never
 * added, or removed by an admin. A device this client SAW removed from the
 * chat is remembered per chat and never re-admitted by it; a removal that
 * happened before this client joined is invisible to it, and the device would
 * be re-admitted — recorded in the ADR as the arm's known limit.
 */

/** A device row from the directory, as fetched. */
export interface DirectoryDevice {
  readonly deviceKeyB64: string;
  readonly keyPackage: Uint8Array;
}

/** A roster leaf with the person it resolves to. */
export interface RosterLeafKeys {
  readonly leafKeyB64: string;
  readonly personKeyB64: string;
}

/** One account in the chat with its directory list; `null` when the fetch
 *  failed — nothing is decided for that account. */
export interface AccountDevices {
  readonly personKeyB64: string;
  readonly devices: readonly DirectoryDevice[] | null;
}

export interface ReadmissionPlan {
  /** KeyPackages to propose an Add for. */
  readonly admit: readonly DirectoryDevice[];
  /** Own-account leaf keys to propose a Remove for. */
  readonly retire: readonly string[];
}

/** How often one chat is re-checked, per account in it. */
export const READMISSION_INTERVAL_MS = 10 * 60_000;

export function planReadmission(input: {
  readonly roster: readonly RosterLeafKeys[];
  readonly ownPersonKeyB64: string;
  readonly accounts: readonly AccountDevices[];
  readonly removed: ReadonlySet<string>;
}): ReadmissionPlan {
  const rosterKeys = new Set(input.roster.map((l) => l.leafKeyB64));
  const admit: DirectoryDevice[] = [];
  const retire: string[] = [];
  const seen = new Set<string>();
  for (const account of input.accounts) {
    if (account.devices === null) continue;
    const listed = new Set(account.devices.map((d) => d.deviceKeyB64));
    for (const d of account.devices) {
      if (rosterKeys.has(d.deviceKeyB64)) continue;
      if (input.removed.has(d.deviceKeyB64)) continue;
      if (seen.has(d.deviceKeyB64)) continue;
      seen.add(d.deviceKeyB64);
      admit.push(d);
    }
    if (account.personKeyB64 !== input.ownPersonKeyB64) continue;
    for (const leaf of input.roster) {
      if (leaf.personKeyB64 !== input.ownPersonKeyB64) continue;
      if (listed.has(leaf.leafKeyB64)) continue;
      retire.push(leaf.leafKeyB64);
    }
  }
  return { admit, retire };
}

// ---- the throttle and the removed-device memory ---------------------------

const lastCheckedAt = new Map<string, number>();

/** Whether `groupIdB64` is due for a check at `nowMs`; stamps it when so. */
export function takeReadmissionSlot(groupIdB64: string, nowMs: number): boolean {
  const last = lastCheckedAt.get(groupIdB64);
  if (last !== undefined && nowMs - last < READMISSION_INTERVAL_MS) return false;
  lastCheckedAt.set(groupIdB64, nowMs);
  return true;
}

/** Tests and sign-out. */
export function clearReadmissionSlots(): void {
  lastCheckedAt.clear();
}

const REMOVED_KEY_PREFIX = "lakko.readmission.removed.v1:";

function removedKey(accountIdB64: string): string {
  return REMOVED_KEY_PREFIX + accountIdB64;
}

function loadRemoved(accountIdB64: string): Record<string, string[]> {
  try {
    const raw = localStorage.getItem(removedKey(accountIdB64));
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    return typeof parsed === "object" && parsed !== null
      ? (parsed as Record<string, string[]>)
      : {};
  } catch {
    return {};
  }
}

/** Remember the leaves that left `groupIdB64` between two roster reads. A
 *  torn read on either side records nothing. */
export function recordRemovedDevices(
  accountIdB64: string,
  groupIdB64: string,
  before: readonly string[] | null,
  after: readonly string[] | null,
): void {
  if (before === null || after === null) return;
  const now = new Set(after);
  const gone = before.filter((k) => !now.has(k));
  if (gone.length === 0) return;
  try {
    const all = loadRemoved(accountIdB64);
    const cur = new Set(all[groupIdB64] ?? []);
    for (const k of gone) cur.add(k);
    all[groupIdB64] = [...cur];
    localStorage.setItem(removedKey(accountIdB64), JSON.stringify(all));
  } catch {
    /* storage unavailable: the memory is per session then */
  }
}

export function removedDevices(accountIdB64: string, groupIdB64: string): Set<string> {
  return new Set(loadRemoved(accountIdB64)[groupIdB64] ?? []);
}

export function clearRemovedDevicesForAccount(accountIdB64: string): void {
  try {
    localStorage.removeItem(removedKey(accountIdB64));
  } catch {
    /* nothing to clear */
  }
}
