// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 3 - the owner appoints registrars.
 *
 * The owner of a conversation group keeps the Delivery Service's registrar
 * set equal to the group: every non-owner account in the roster is a
 * registrar, at `admin` rank when the ADR-0022 role table names it an admin
 * and at `member` rank otherwise. Each appointed member also needs its seed,
 * which the owner sends inside the group as a registrar-seed envelope sealed
 * to the member's ACCOUNT X25519 key (from its identity bundle, whose
 * signature key must be the member's account key - a discovery server cannot
 * substitute its own box key).
 *
 * ## Converge, do not diff (the second security pass, finding 2)
 *
 * The owner no longer sends changes computed against a device-local ledger:
 * a ledger that missed a removal (another owner device, a lost record) left
 * an evicted member's registrar key on the DS for good. It sends
 * `registrars-set` carrying the WHOLE desired set derived from the live
 * roster and role table; the DS drops every registrar not listed. It is sent
 * whenever that set differs from the one this session last sent for the
 * group, so once per group per session and on every roster or role change;
 * an identical set is a 204. Every owner device converges the DS to the group
 * on its own, with nothing to agree on.
 *
 * The local record now only tracks which DEVICES have their seed envelope
 * (and at which rank), keyed on each device's leaf signature key (ADR-0057,
 * "Who revokes, and who holds a registrar seed", item 2): the envelope is
 * sealed to the ACCOUNT, so one envelope serves every device of the account
 * that is in the group when it is posted, but a device that joins later never
 * sees it. Keyed per device, the record shows that device as owed, and the
 * owner's next reconcile sends the account a fresh envelope (the same seed),
 * which the new device reads. An account whose X25519 key cannot be resolved yet
 * (its profile, which carries the account id, has not arrived) is registered
 * all the same - registering a key hands nothing to anyone who lacks the seed
 * - and its envelope is left PENDING, retried next time. A re-sent envelope
 * carries the same seed.
 */
import {
  concatKeys,
  desiredRegistrars,
  releaseAuthority,
  sendStatement,
  type JournalAuthority,
  type RegistrarDerivation,
  type StatementSender,
  type StatementSignerLike,
} from "./ownership";

/** The most keys one `registrars` statement may carry (the server's bound). */
export const MAX_REGISTRAR_CHANGES = 256;

/** The most entries one `registrars-set` statement may carry. */
export const MAX_REGISTRAR_SET = 4096;

/** The group as the reconcile reads it, now. */
export interface AppointmentRoster {
  /** base64 of every member's ACCOUNT key (one per person, ADR-0050). */
  readonly memberAccountKeysB64: readonly string[];
  /** The role table's owner, base64; `undefined` for a table-less group. */
  readonly ownerAccountKeyB64: string | undefined;
  /** The role table's admins, base64. */
  readonly adminAccountKeysB64: ReadonlySet<string>;
  /** base64 account key -> base64 leaf signature keys of that account's
   *  devices in the roster (ADR-0050). An account missing here (or the whole
   *  map absent) is one device whose leaf key is the account key (Phase 1). */
  readonly deviceKeysByAccountB64?: ReadonlyMap<string, readonly string[]>;
}

export interface AppointmentPorts {
  readonly groupId: Uint8Array;
  /** The live roster and role table, or `null` when unreadable. */
  roster(): AppointmentRoster | null;
  /** This account's key, base64, or `null` when signed out. */
  ownAccountKeyB64(): string | null;
  /** This device's authority over the journal; released by the reconcile. */
  authority(): JournalAuthority;
  readonly derive: RegistrarDerivation;
  readonly sender: StatementSender;
  /** Which DEVICES hold their account's seed envelope, at which rank (base64
   *  device leaf signature key -> rank). */
  loadRecord(): Map<string, number>;
  saveRecord(record: ReadonlyMap<string, number>): void;
  /** The fingerprint of the registrar set this session last sent for the
   *  group, or `null` (never sent this session). */
  lastSentSet(): string | null;
  noteSentSet(fingerprint: string): void;
  /** The member account's X25519 key from its identity bundle (verified to
   *  belong to that account key), or `null` when not resolvable yet. */
  memberX25519(accountKeyB64: string): Promise<Uint8Array | null>;
  /** `composeRegistrarSeedEnvelope(journalId, memberAccountKey, x25519, rank,
   *  seed)`. */
  composeEnvelope(
    memberAccountKey: Uint8Array,
    memberX25519: Uint8Array,
    rank: number,
    seed: Uint8Array,
  ): Uint8Array;
  /** Seal and publish one envelope into the group (an MLS application
   *  message that wakes nobody). */
  sendEnvelope(envelope: Uint8Array): Promise<void>;
}

export type AppointmentOutcome =
  | { readonly kind: "unavailable" }
  | { readonly kind: "not-owner" }
  | { readonly kind: "unchanged" }
  | {
      readonly kind: "reconciled";
      /** `true` when a `registrars-set` statement went out this pass. */
      readonly setSent: boolean;
      /** Accounts sent their seed envelope this round. */
      readonly appointed: readonly string[];
      /** Accounts whose envelope is left for next time (no X25519 key yet,
       *  or a failed send). */
      readonly pending: readonly string[];
    };

function fromB64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** A deterministic fingerprint of a desired set (sorted key:rank pairs). */
export function registrarSetFingerprint(desired: ReadonlyMap<string, number>): string {
  return [...desired.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([k, r]) => `${k}:${r}`)
    .join(",");
}

/**
 * Sign and send `registrars-set` for `desired` (base64 account key -> rank)
 * with the root: each entry is the registrar PUBLIC key derived from the
 * root seed for that account, never the account key. Entries are sorted by
 * account key so the statement is deterministic. Throws the DS refusal.
 */
export async function sendRegistrarSet(
  sender: StatementSender,
  journalId: Uint8Array,
  root: { readonly signer: Pick<StatementSignerLike, "registrarsSet">; readonly rootSeed: Uint8Array },
  desired: ReadonlyMap<string, number>,
  derive: RegistrarDerivation,
): Promise<void> {
  const entries = [...desired.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  if (entries.length > MAX_REGISTRAR_SET) {
    throw new Error("ds ownership: the registrar set exceeds the server bound");
  }
  const publicKeys: Uint8Array[] = [];
  for (const [key] of entries) {
    const seed = derive.registrarSeed(root.rootSeed, fromB64(key));
    try {
      publicKeys.push(derive.publicKeyOfSeed(seed));
    } finally {
      seed.fill(0);
    }
  }
  const keys = concatKeys(publicKeys);
  const ranks = Uint8Array.from(entries.map(([, rank]) => rank));
  await sendStatement(sender, () => root.signer.registrarsSet(journalId, keys, ranks));
}

/** The desired registrar set of `roster` for the owner `own`, or `null` when
 *  `own` is not the roster's owner. */
export function desiredSetFor(
  roster: AppointmentRoster,
  own: string,
): Map<string, number> | null {
  if (roster.ownerAccountKeyB64 === undefined || roster.ownerAccountKeyB64 !== own) return null;
  return desiredRegistrars({
    memberAccountKeysB64: roster.memberAccountKeysB64,
    ownerAccountKeyB64: own,
    adminAccountKeysB64: roster.adminAccountKeysB64,
  });
}

/**
 * One reconcile pass: `registrars-set` with the full desired set when it
 * differs from what this session last sent, then one envelope per account
 * whose recorded rank differs from the desired one. A refused statement
 * throws (nothing recorded, the next pass retries); a failed envelope leaves
 * only that member pending.
 */
export async function reconcileAppointments(ports: AppointmentPorts): Promise<AppointmentOutcome> {
  const roster = ports.roster();
  const own = ports.ownAccountKeyB64();
  if (roster === null || own === null) return { kind: "unavailable" };
  const desired = desiredSetFor(roster, own);
  if (desired === null) return { kind: "not-owner" };

  const fingerprint = registrarSetFingerprint(desired);
  const setDue = ports.lastSentSet() !== fingerprint;
  const record = ports.loadRecord();
  const devicesOf = (account: string): readonly string[] => {
    const devices = roster.deviceKeysByAccountB64?.get(account);
    return devices === undefined || devices.length === 0 ? [account] : devices;
  };
  // device -> the rank its account should hold, for every device in the group.
  const wantedByDevice = new Map<string, number>();
  for (const [account, rank] of desired) {
    for (const device of devicesOf(account)) wantedByDevice.set(device, rank);
  }
  // An account is owed an envelope while ANY of its devices lacks one at the
  // desired rank: a device that joined after the last envelope was posted.
  const owed = [...desired.entries()].filter(([account, rank]) =>
    devicesOf(account).some((device) => record.get(device) !== rank),
  );
  const departed = [...record.keys()].some((device) => !wantedByDevice.has(device));
  if (!setDue && owed.length === 0 && !departed) return { kind: "unchanged" };

  const authority = ports.authority();
  try {
    if (authority.kind !== "root") return { kind: "not-owner" };

    if (setDue) {
      await sendRegistrarSet(ports.sender, ports.groupId, authority, desired, ports.derive);
      ports.noteSentSet(fingerprint);
    }

    const next = new Map<string, number>();
    for (const [device, rank] of record) {
      if (wantedByDevice.get(device) === rank) next.set(device, rank);
    }
    const appointed: string[] = [];
    const pending: string[] = [];
    for (const [key, rank] of owed) {
      let x25519: Uint8Array | null = null;
      try {
        x25519 = await ports.memberX25519(key);
      } catch {
        x25519 = null;
      }
      if (x25519 === null) {
        pending.push(key);
        continue;
      }
      const seed = ports.derive.registrarSeed(authority.rootSeed, fromB64(key));
      try {
        const envelope = ports.composeEnvelope(fromB64(key), x25519, rank, seed);
        await ports.sendEnvelope(envelope);
        // Every device of the account in the group now can read it.
        for (const device of devicesOf(key)) next.set(device, rank);
        appointed.push(key);
      } catch (err) {
        // The seed is never logged; the failure is.
        // eslint-disable-next-line no-console
        console.warn("ds: registrar seed envelope not sent; retried later", err instanceof Error ? err.name : "error");
        pending.push(key);
      } finally {
        seed.fill(0);
      }
    }
    ports.saveRecord(next);
    return { kind: "reconciled", setSent: setDue, appointed, pending };
  } finally {
    releaseAuthority(authority);
  }
}

// ----------------------------------------------------------------------------
// The record (which accounts hold their seed envelope), in localStorage
// ----------------------------------------------------------------------------

/** v2: keyed per device leaf, not per account (no deployment carries v1). */
const RECORD_PREFIX = "lakko/ds-appointments/v2/";

/** The storage the record uses; injectable for tests. Public keys and ranks
 *  only - nothing secret. */
export interface RecordStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

function defaultStorage(): RecordStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

export function loadAppointmentRecord(
  accountId: string,
  groupIdB64: string,
  storage: RecordStorage | null = defaultStorage(),
): Map<string, number> {
  const out = new Map<string, number>();
  if (storage === null) return out;
  try {
    const raw = storage.getItem(`${RECORD_PREFIX}${accountId}/${groupIdB64}`);
    if (raw === null) return out;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return out;
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (v === 1 || v === 2) out.set(k, v);
    }
  } catch {
    /* unreadable: start from nothing, which re-appoints everyone once */
  }
  return out;
}

export function saveAppointmentRecord(
  accountId: string,
  groupIdB64: string,
  record: ReadonlyMap<string, number>,
  storage: RecordStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    storage.setItem(
      `${RECORD_PREFIX}${accountId}/${groupIdB64}`,
      JSON.stringify(Object.fromEntries(record)),
    );
  } catch {
    /* best effort: a lost record costs one re-appointment */
  }
}

/** Forget what this device appointed for one group (ADR-0057 root handover:
 *  the registrars of the root it appointed under are gone). */
export function clearAppointmentRecord(
  accountId: string,
  groupIdB64: string,
  storage: RecordStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    if (storage.removeItem !== undefined) storage.removeItem(`${RECORD_PREFIX}${accountId}/${groupIdB64}`);
    else storage.setItem(`${RECORD_PREFIX}${accountId}/${groupIdB64}`, "{}");
  } catch {
    /* best effort */
  }
}

/** Forget one account's records (forget device). */
export function clearAppointmentRecords(accountId: string, storage: RecordStorage | null = defaultStorage()): void {
  if (storage === null) return;
  try {
    const ls = storage as unknown as Storage;
    if (typeof ls.length !== "number" || typeof ls.key !== "function") return;
    const doomed: string[] = [];
    for (let i = 0; i < ls.length; i++) {
      const k = ls.key(i);
      if (k !== null && k.startsWith(`${RECORD_PREFIX}${accountId}/`)) doomed.push(k);
    }
    for (const k of doomed) storage.removeItem?.(k);
  } catch {
    /* best effort */
  }
}
