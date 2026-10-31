// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 "Who revokes, and who holds a registrar seed" (amended 2026-10-01),
 * item 1 - the registrar list every member collects, so whoever removes a
 * person revokes that person's registrar key in the same registration.
 *
 * The owner appoints registrars with a registrar-seed envelope per account
 * (`appointments.ts`). Its clear header names the addressee's account key, the
 * rank and the registrar PUBLIC key (public anyway: the DS holds it). Every
 * member reads every such envelope in the group, so every member can keep
 * `addressee account key -> (registrar public key, rank)` from them, whoever
 * the envelope is addressed to. An admin (no root seed, so it cannot derive
 * the keys itself) that commits a removal looks the evicted accounts up here
 * and revokes their keys in its `access` statement.
 *
 * Kept only from an envelope whose MLS sender resolved to the group's owner at
 * receipt (C-68), for this journal, at rank member or admin: a member cannot
 * plant an entry that makes an admin revoke someone else's key. The lookup at
 * removal time is `revocationKeys` in `ownership.ts`.
 *
 * Public keys and ranks only, so localStorage, per (account, group). A newer
 * envelope for the same account replaces the entry (the owner re-sends on a
 * promotion or demotion).
 */
import { RANK_ADMIN, RANK_MEMBER } from "./ownership";

/** One collected entry. */
export interface RegistrarListEntry {
  /** The registrar's 32-byte Ed25519 public key. */
  readonly publicKey: Uint8Array;
  /** `RANK_MEMBER` or `RANK_ADMIN`. */
  readonly rank: number;
}

/** What one decoded envelope says, and who sent it. */
export interface RegistrarListArrival {
  /** The decoded clear header, or `null` when it did not decode. */
  readonly header: {
    readonly journalId: Uint8Array;
    readonly memberAccountKey: Uint8Array;
    readonly rank: number;
    readonly registrarPublicKey?: Uint8Array;
  } | null;
  readonly groupId: Uint8Array;
  /** The MLS sender's account key, resolved at receipt. */
  readonly senderAccountKey: Uint8Array | undefined;
  /** The live role table's owner, read at receipt. */
  readonly ownerAccountKey: Uint8Array | undefined;
}

function sameBytes(a: Uint8Array | undefined, b: Uint8Array | undefined): boolean {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

function fromB64(s: string): Uint8Array | null {
  try {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/**
 * The entry an envelope contributes, or `null`: only from the owner, only for
 * this journal, only a member or admin registrar (a rank-3 root handover names
 * the root, which is not a registrar and is never revoked by a member).
 */
export function registrarListEntryOf(
  arrival: RegistrarListArrival,
): { readonly accountKeyB64: string; readonly entry: RegistrarListEntry } | null {
  const h = arrival.header;
  if (h === null) return null;
  if (h.rank !== RANK_MEMBER && h.rank !== RANK_ADMIN) return null;
  if (!sameBytes(h.journalId, arrival.groupId)) return null;
  if (!sameBytes(arrival.senderAccountKey, arrival.ownerAccountKey)) return null;
  const pk = h.registrarPublicKey;
  if (pk === undefined || pk.length !== 32 || h.memberAccountKey.length !== 32) return null;
  return { accountKeyB64: b64(h.memberAccountKey), entry: { publicKey: pk.slice(), rank: h.rank } };
}

// ----------------------------------------------------------------------------
// Persistence
// ----------------------------------------------------------------------------

const PREFIX = "lakko/ds-registrar-list/v1/";

/** The storage the list uses; injectable for tests. */
export interface RegistrarListStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

function defaultStorage(): RegistrarListStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Read one group's list. Unreadable rows read as empty: an admin then
 *  revokes nothing, and the owner's next reconcile removes the key. */
export function loadRegistrarList(
  accountId: string,
  groupIdB64: string,
  storage: RegistrarListStorage | null = defaultStorage(),
): Map<string, RegistrarListEntry> {
  const out = new Map<string, RegistrarListEntry>();
  if (storage === null) return out;
  try {
    const raw = storage.getItem(`${PREFIX}${accountId}/${groupIdB64}`);
    if (raw === null) return out;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return out;
    for (const [account, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (v === null || typeof v !== "object") continue;
      const { k, r } = v as { k?: unknown; r?: unknown };
      if (typeof k !== "string" || (r !== RANK_MEMBER && r !== RANK_ADMIN)) continue;
      const publicKey = fromB64(k);
      if (publicKey === null || publicKey.length !== 32) continue;
      out.set(account, { publicKey, rank: r });
    }
  } catch {
    /* unreadable: empty */
  }
  return out;
}

/** Record one entry (replacing the account's previous one). Best effort. */
export function recordRegistrarListEntry(
  accountId: string,
  groupIdB64: string,
  accountKeyB64: string,
  entry: RegistrarListEntry,
  storage: RegistrarListStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  const list = loadRegistrarList(accountId, groupIdB64, storage);
  list.set(accountKeyB64, entry);
  const obj: Record<string, { k: string; r: number }> = {};
  for (const [a, e] of list) obj[a] = { k: b64(e.publicKey), r: e.rank };
  try {
    storage.setItem(`${PREFIX}${accountId}/${groupIdB64}`, JSON.stringify(obj));
  } catch {
    /* best effort: a lost entry costs one revocation the owner backstops */
  }
}

/** Forget one group's list (ADR-0087 section 9). */
export function clearRegistrarList(
  accountId: string,
  groupIdB64: string,
  storage: RegistrarListStorage | null = defaultStorage(),
): void {
  try {
    storage?.removeItem?.(`${PREFIX}${accountId}/${groupIdB64}`);
  } catch {
    /* best effort */
  }
}

/** Forget one account's lists (forget device). */
export function clearRegistrarLists(
  accountId: string,
  storage: RegistrarListStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    const ls = storage as unknown as Storage;
    if (typeof ls.length !== "number" || typeof ls.key !== "function") return;
    const doomed: string[] = [];
    for (let i = 0; i < ls.length; i++) {
      const k = ls.key(i);
      if (k !== null && k.startsWith(`${PREFIX}${accountId}/`)) doomed.push(k);
    }
    for (const k of doomed) storage.removeItem?.(k);
  } catch {
    /* best effort */
  }
}

// ----------------------------------------------------------------------------
// The signed-in account
// ----------------------------------------------------------------------------

let boundAccount: string | null = null;

/** Bind (or unbind, with `null`) the signed-in account; `JournalOwnershipContext`
 *  does, alongside the registrar-seed store. */
export function bindRegistrarListAccount(accountId: string | null): void {
  boundAccount = accountId;
}

/** The signed-in account's list for a group (empty when signed out). */
export function registrarListFor(groupIdB64: string): Map<string, RegistrarListEntry> {
  return boundAccount === null ? new Map() : loadRegistrarList(boundAccount, groupIdB64);
}

/** Record an envelope's entry for the signed-in account, when it yields one.
 *  Returns whether it did. Never throws. */
export function noteRegistrarListArrival(groupIdB64: string, arrival: RegistrarListArrival): boolean {
  if (boundAccount === null) return false;
  const got = registrarListEntryOf(arrival);
  if (got === null) return false;
  recordRegistrarListEntry(boundAccount, groupIdB64, got.accountKeyB64, got.entry);
  return true;
}
