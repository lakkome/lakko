// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - what this device remembers about the owned-channels
 * list between sessions, per account, in `localStorage`. Channel ids and
 * flags only: the list key `R`, the server half `S`, every root and every
 * seed stay on the storage service and in memory while used.
 *
 * - **registered** ({@link readRecoveryRegistered}): the owned channels whose
 *   recovery key, derived from `R`, this device registered with the channels
 *   service. The key name carries `v2`: a channel registered under the
 *   removed OPRF-derived key (the `lakko:channel-recovery-registered:` rows
 *   of 2026-11-25) is registered again.
 * - **owed deletes** ({@link readOwedListDeletes}): channels this device
 *   deleted whose entry has not left the list yet.
 * - **retake** ({@link readRetake}): a key reset on this device took the
 *   list back and has entries left to take; the publisher pass finishes it.
 * - **records owed** ({@link readRetakenRecordsOwed}): retaken channels
 *   whose record (name, description, photo) has not been posted again under
 *   the re-founded group's epoch yet.
 *
 * Removed by sign-out with Forget device ({@link clearOwnedChannelsFacts}).
 */

/** The part of `Storage` used here (tests pass their own). */
export interface FactsStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** The browser's `localStorage`, or `undefined` where there is none. */
export function factsStorage(): FactsStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

const REGISTERED = "lakko:channel-recovery-registered-v2:";
/** The rows of the removed OPRF-derived recovery key. Never read. */
const REGISTERED_RETIRED = "lakko:channel-recovery-registered:";
const DELETES = "lakko:owned-channels-deletes:";
const RETAKE = "lakko:owned-channels-retake:";
const RECORDS = "lakko:owned-channels-records-owed:";

function readIds(s: FactsStorage | undefined, key: string): string[] {
  try {
    const raw = s?.getItem(key);
    const parsed = raw === null || raw === undefined ? [] : (JSON.parse(raw) as unknown);
    return Array.isArray(parsed)
      ? parsed.filter((x): x is string => typeof x === "string" && /^[0-9a-f]{32}$/.test(x))
      : [];
  } catch {
    return [];
  }
}

function writeIds(s: FactsStorage | undefined, key: string, ids: Iterable<string>): void {
  const sorted = [...new Set(ids)].sort();
  if (sorted.length === 0) s?.removeItem(key);
  else s?.setItem(key, JSON.stringify(sorted));
}

// ----------------------------------------------------------------------------
// Recovery keys registered
// ----------------------------------------------------------------------------

/** The owned channels whose list-key recovery key landed, per account. */
export function readRecoveryRegistered(s: FactsStorage | undefined, accountIdB64: string): Set<string> {
  return new Set(readIds(s, REGISTERED + accountIdB64));
}

/** One more landed. */
export function markRecoveryRegistered(s: FactsStorage | undefined, accountIdB64: string, idHex: string): void {
  try {
    writeIds(s, REGISTERED + accountIdB64, [...readRecoveryRegistered(s, accountIdB64), idHex.toLowerCase()]);
  } catch {
    /* registered again next pass: the PUT is idempotent */
  }
}

/** Wipe (the retired OPRF rows of the account too). */
export function clearRecoveryRegistered(s: FactsStorage | undefined, accountIdB64: string): void {
  for (const prefix of [REGISTERED, REGISTERED_RETIRED]) {
    try {
      s?.removeItem(prefix + accountIdB64);
    } catch {
      /* nothing recorded either */
    }
  }
}

/** The owned channels still owing a registration. Pure. */
export function channelsOwingRecoveryKey(owned: readonly string[], registered: ReadonlySet<string>): string[] {
  return [...new Set(owned.map((id) => id.toLowerCase()))].filter((id) => !registered.has(id)).sort();
}

// ----------------------------------------------------------------------------
// Entries owed out of the list
// ----------------------------------------------------------------------------

/** The channels this device deleted whose entry is still to leave the list. */
export function readOwedListDeletes(s: FactsStorage | undefined, accountIdB64: string): string[] {
  return readIds(s, DELETES + accountIdB64);
}

/** A channel was deleted here: its entry is owed out of the list. */
export function oweListDelete(s: FactsStorage | undefined, accountIdB64: string, idHex: string): void {
  try {
    writeIds(s, DELETES + accountIdB64, [...readOwedListDeletes(s, accountIdB64), idHex.toLowerCase()]);
  } catch {
    /* the entry stays listed: a retake of it answers 410 and drops it */
  }
}

/** The list no longer carries `settled`. */
export function settleListDeletes(s: FactsStorage | undefined, accountIdB64: string, settled: readonly string[]): void {
  const done = new Set(settled.map((id) => id.toLowerCase()));
  try {
    writeIds(
      s,
      DELETES + accountIdB64,
      readOwedListDeletes(s, accountIdB64).filter((id) => !done.has(id)),
    );
  } catch {
    /* written out again next pass, which changes nothing */
  }
}

// ----------------------------------------------------------------------------
// The retake after a key reset
// ----------------------------------------------------------------------------

/** A retake in progress: the entries already settled (taken or dropped). */
export interface RetakeRecord {
  readonly done: readonly string[];
}

/** The retake owed on this device for the account, or `null`. */
export function readRetake(s: FactsStorage | undefined, accountIdB64: string): RetakeRecord | null {
  try {
    const raw = s?.getItem(RETAKE + accountIdB64);
    if (raw === null || raw === undefined) return null;
    const parsed = JSON.parse(raw) as { done?: unknown };
    const done = Array.isArray(parsed.done)
      ? parsed.done.filter((x): x is string => typeof x === "string" && /^[0-9a-f]{32}$/.test(x))
      : [];
    return { done };
  } catch {
    return null;
  }
}

/** A retake starts (an earlier record of the account is kept). Throws when
 *  storage refuses, so the caller reports a retry instead of a retake that
 *  a reload would forget. */
export function startRetake(s: FactsStorage | undefined, accountIdB64: string): void {
  if (s === undefined) throw new Error("no storage for the owned-channels retake");
  if (readRetake(s, accountIdB64) !== null) return;
  s.setItem(RETAKE + accountIdB64, JSON.stringify({ done: [] }));
}

/** One entry is settled. */
export function noteRetakeDone(s: FactsStorage | undefined, accountIdB64: string, idHex: string): void {
  const held = readRetake(s, accountIdB64);
  if (held === null) return;
  try {
    s?.setItem(RETAKE + accountIdB64, JSON.stringify({ done: [...new Set([...held.done, idHex.toLowerCase()])].sort() }));
  } catch {
    /* settled again next pass: every step is idempotent */
  }
}

/** Every entry is settled and the locks are in order. */
export function finishRetake(s: FactsStorage | undefined, accountIdB64: string): void {
  try {
    s?.removeItem(RETAKE + accountIdB64);
  } catch {
    /* finished again next pass */
  }
}

// ----------------------------------------------------------------------------
// The record of a retaken channel
// ----------------------------------------------------------------------------

/** The retaken channels whose record is still to be posted again. */
export function readRetakenRecordsOwed(s: FactsStorage | undefined, accountIdB64: string): string[] {
  return readIds(s, RECORDS + accountIdB64);
}

/** A channel was retaken: its record is owed under its new epoch. */
export function oweRetakenRecord(s: FactsStorage | undefined, accountIdB64: string, idHex: string): void {
  try {
    writeIds(s, RECORDS + accountIdB64, [...readRetakenRecordsOwed(s, accountIdB64), idHex.toLowerCase()]);
  } catch {
    /* the next rotation posts the record again anyway */
  }
}

/** The record of `idHex` was handed to the publisher pass (or the channel
 *  is gone). */
export function settleRetakenRecord(s: FactsStorage | undefined, accountIdB64: string, idHex: string): void {
  try {
    writeIds(
      s,
      RECORDS + accountIdB64,
      readRetakenRecordsOwed(s, accountIdB64).filter((id) => id !== idHex.toLowerCase()),
    );
  } catch {
    /* handed over again next pass, which changes nothing */
  }
}

// ----------------------------------------------------------------------------
// Wipe
// ----------------------------------------------------------------------------

/**
 * Sign-out with Forget device: everything this module keeps for the account.
 * Never throws.
 */
export function clearOwnedChannelsStoredFacts(s: FactsStorage | undefined, accountIdB64: string): void {
  clearRecoveryRegistered(s, accountIdB64);
  for (const prefix of [DELETES, RETAKE, RECORDS]) {
    try {
      s?.removeItem(prefix + accountIdB64);
    } catch {
      /* nothing recorded either */
    }
  }
}
