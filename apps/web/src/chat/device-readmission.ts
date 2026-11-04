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
 *
 * ADR-0074 section 2.3: for the OWN account that limit is closed by a second
 * test. A device of this account is admitted only when it is in the
 * account's DeviceGroup roster (`devices/own-device-roster.ts`), so a device
 * that was removed and publishes itself again is not put back; the per-chat
 * memory stays as defence in depth.
 */

/** A device row from the directory, as fetched. */
export interface DirectoryDevice {
  readonly deviceKeyB64: string;
  readonly keyPackage: Uint8Array;
  /**
   * ADR-0074 (a device paired after a key rotation): `false` when the listed
   * KeyPackage of an OWN device is bound to another account key than the one
   * this device's leaves are bound to (`devices/own-conversation-key.ts`).
   * The row still counts as listed (its leaf is not retired); it is not
   * admitted until the device publishes one bound to the right key. Absent
   * reads as admissible, and a contact's device is never affected.
   */
  readonly admissible?: boolean;
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
  /**
   * ADR-0074 section 2.3 — the leaf keys (standard base64) of the account's
   * current DeviceGroup roster, or `null` when it is not known. A device of
   * THIS account is admitted only when it is in it: a removed device is not,
   * whatever the directory lists for it. `null` admits no own device (the
   * next check, with the roster loaded, does). Contacts' devices are not
   * affected.
   */
  readonly ownDeviceGroup: ReadonlySet<string> | null;
}): ReadmissionPlan {
  const rosterKeys = new Set(input.roster.map((l) => l.leafKeyB64));
  const admit: DirectoryDevice[] = [];
  const retire: string[] = [];
  const seen = new Set<string>();
  for (const account of input.accounts) {
    if (account.devices === null) continue;
    const own = account.personKeyB64 === input.ownPersonKeyB64;
    const listed = new Set(account.devices.map((d) => d.deviceKeyB64));
    for (const d of account.devices) {
      if (rosterKeys.has(d.deviceKeyB64)) continue;
      if (input.removed.has(d.deviceKeyB64)) continue;
      if (own && (input.ownDeviceGroup === null || !input.ownDeviceGroup.has(d.deviceKeyB64))) continue;
      if (own && d.admissible === false) continue;
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

/** Drop one group's removed leaves (ADR-0087 section 9). */
export function clearRemovedDevicesForGroup(accountIdB64: string, groupIdB64: string): void {
  try {
    const all = loadRemoved(accountIdB64);
    if (!(groupIdB64 in all)) return;
    delete all[groupIdB64];
    localStorage.setItem(removedKey(accountIdB64), JSON.stringify(all));
  } catch {
    /* nothing to clear */
  }
}

export function clearRemovedDevicesForAccount(accountIdB64: string): void {
  try {
    localStorage.removeItem(removedKey(accountIdB64));
  } catch {
    /* nothing to clear */
  }
}

// ---- ADR-0073 R.2: the wider lookup ----------------------------------------

/** A roster person this device can look up in the directory. */
export interface LookupCandidate {
  /** The PERSON key the roster names (the key the leaves are bound to). */
  readonly rosterKeyB64: string;
  /** The account id this device knows that person under, 32 hex chars. */
  readonly accountIdHex: string;
}

/** The slice of a contact row the lookup reads. */
export interface LookupContact {
  readonly signaturePubkeyB64: string;
  readonly previousSignaturePubkeysB64?: readonly string[];
  readonly accountIdHex?: string;
}

/**
 * ADR-0073 R.2 "Triggers" — every roster account whose account id this
 * device knows, not only its accepted contacts: a contact row (found by its
 * current key or any key it had), else the profile store (the profile
 * envelope carries the account id, ADR-0048 v2). This is also what removes
 * G-44's "no admin is the person's contact" case.
 *
 * The contact row wins over the profile's claim. The own account is not a
 * candidate (the caller looks itself up with its own id).
 */
export function readmissionCandidates(input: {
  readonly rosterPersonKeysB64: readonly string[];
  readonly ownPersonKeyB64: string;
  readonly contacts: readonly LookupContact[];
  /** `profiles.accountIdHexOf`: the account id a member's own profile
   *  claims, or `null`. */
  readonly profileAccountIdHexOf: (personKeyB64: string) => string | null;
}): LookupCandidate[] {
  const out: LookupCandidate[] = [];
  const seen = new Set<string>();
  for (const rosterKeyB64 of input.rosterPersonKeysB64) {
    if (rosterKeyB64 === input.ownPersonKeyB64 || seen.has(rosterKeyB64)) continue;
    seen.add(rosterKeyB64);
    const row = input.contacts.find(
      (c) =>
        c.signaturePubkeyB64 === rosterKeyB64 ||
        (c.previousSignaturePubkeysB64 ?? []).includes(rosterKeyB64),
    );
    const accountIdHex = row?.accountIdHex ?? input.profileAccountIdHexOf(rosterKeyB64);
    if (accountIdHex === null || accountIdHex === undefined) continue;
    if (!/^[0-9a-f]{32}$/i.test(accountIdHex)) continue;
    out.push({ rosterKeyB64, accountIdHex: accountIdHex.toLowerCase() });
  }
  return out;
}

/** How the roster's key for a person stands against what this device trusts
 *  for the account AFTER the identity check ran. */
export type LookupStanding =
  /** The roster key is the account's current key or a PROVEN predecessor of
   *  it: the ordinary ADR-0050 re-admission (a listed device with no leaf). */
  | "same-person"
  /** The roster key was retired by an ACCEPTED RESET of the account: the
   *  directory now lists the new identity's devices (R.7 case 2). */
  | "reset-identity"
  /** Anything else (no pin, an unrelated key): the account's devices are
   *  not admitted under this person. */
  | "skip";

/**
 * | pinned | roster key is ...                               | standing         |
 * |--------|-------------------------------------------------|------------------|
 * | yes    | the pin, or a proven predecessor of the pin     | `same-person`    |
 * | yes    | a key an accepted reset of this account retired | `reset-identity` |
 * | yes    | anything else                                   | `skip`           |
 * | no     | the key of this device's VERIFIED BINDING, or   | `same-person`    |
 * |        | the conversation key a link proved for it       |                  |
 * | no     | anything else, or no binding                    | `skip`           |
 *
 * `skip` is what stops a member's own profile from attaching another
 * account's devices to itself: an account id is only followed to devices
 * when the roster's key is one this device TRUSTS for that account: the
 * pin record, or the binding it verified itself against the directory
 * (`contacts/verified-binding.ts`). A claimed id alone is never followed.
 */
export function lookupStanding(input: {
  readonly rosterKeyB64: string;
  readonly pin:
    | {
        readonly pinnedSignaturePubkeyB64: string;
        readonly provenPredecessorsB64?: readonly string[];
        readonly supersededKeysB64?: readonly string[];
      }
    | undefined;
  /** The verified binding of the account when it is NOT pinned. */
  readonly binding?:
    | { readonly keyB64: string; readonly conversationKeyB64?: string | undefined }
    | undefined;
}): LookupStanding {
  const pin = input.pin;
  if (pin === undefined) {
    const binding = input.binding;
    return binding !== undefined &&
      (binding.keyB64 === input.rosterKeyB64 || binding.conversationKeyB64 === input.rosterKeyB64)
      ? "same-person"
      : "skip";
  }
  if (
    pin.pinnedSignaturePubkeyB64 === input.rosterKeyB64 ||
    (pin.provenPredecessorsB64 ?? []).includes(input.rosterKeyB64)
  ) {
    return "same-person";
  }
  return (pin.supersededKeysB64 ?? []).includes(input.rosterKeyB64) ? "reset-identity" : "skip";
}
