// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Resolve before you address (ADR-0066 section 3.5a, rules 1, 2 and 4).
 *
 * A mailbox's address and the right to read it both derive from an account
 * key. After a PROVEN key rotation the conversation leaves that exist stay
 * bound to the old key, so every place that turns an account key into a
 * mailbox (a Welcome for a leaf, a contact request, a call cold start, a
 * committer delegation, another device of the own account) would address the
 * mailbox the holder of the old seed can read. This module is the one rule
 * that maps a key to the CURRENT key of the person it names before anything
 * is addressed:
 *
 *   - a proven predecessor resolves to the key that superseded it, through
 *     any chain, for a pinned account and for the OWN account alike;
 *   - a key change WITHOUT a proof (a reset, ADR-0065) is never resolved:
 *     the pin store keeps no proven predecessor for it, so the key names
 *     itself;
 *   - a key claimed as a predecessor by two accounts names nobody.
 *
 * The same table answers rule 2: the keys one person has had, current first,
 * so a reach grant filed under a key that later became a proven predecessor
 * is still found.
 *
 * And rule 4, the pure read window of a predecessor mailbox.
 *
 * The pure functions take their inputs; the runtime half reads the pin store
 * (`local-store.ts`, synchronous) and the own account's keys, installed by
 * `ReachProvider` when the session's identities are set.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadAccountKeys } from "./local-store";
import { buildPredecessorTable, currentKeyOf, type AccountKeys } from "./predecessor";

// ---------------------------------------------------------------------------
// Pure
// ---------------------------------------------------------------------------

function everyAccount(pinned: readonly AccountKeys[], own: AccountKeys | null): AccountKeys[] {
  return own === null ? [...pinned] : [own, ...pinned];
}

/**
 * The account key to address for the person `keyB64` names: the current key
 * when `keyB64` is a PROVEN predecessor of a pinned account or of the own
 * account, else `keyB64` itself.
 */
export function resolveBeforeAddress(
  keyB64: string,
  pinned: readonly AccountKeys[],
  own: AccountKeys | null,
): string {
  return currentKeyOf(keyB64, buildPredecessorTable(everyAccount(pinned, own)));
}

/**
 * Every key the person `keyB64` names has had, as far as this device has
 * proof: the current key first, then the proven predecessors newest first.
 * A key no account is known under is its own only key.
 */
export function keysOfPerson(
  keyB64: string,
  pinned: readonly AccountKeys[],
  own: AccountKeys | null,
): string[] {
  const accounts = everyAccount(pinned, own);
  const table = buildPredecessorTable(accounts);
  const current = currentKeyOf(keyB64, table);
  const out = [current];
  for (const account of accounts) {
    if (account.currentKeyB64 !== current) continue;
    for (let i = account.provenPredecessorsB64.length - 1; i >= 0; i--) {
      const old = account.provenPredecessorsB64[i];
      // Only a key the table really maps here (an ambiguous one names nobody).
      if (old !== undefined && table.get(old) === current && !out.includes(old)) out.push(old);
    }
  }
  return out;
}

/** How long a predecessor mailbox is read after adopting the new keys: the
 *  DS journal retention (ADR-0066 section 3.5a rule 4; chosen). */
export const PREDECESSOR_MAILBOX_READ_MS = 30 * 24 * 60 * 60 * 1000;

/**
 * Whether a predecessor mailbox is still read (rule 4). Read-only, for 30
 * days from the adoption; nothing is ever registered or written there.
 *
 * `adoptedAtMs` is `null` when no adoption time is stored: the caller stamps
 * one (the window then starts now) and the mailbox is read meanwhile, so a
 * device that adopted before the time was recorded loses nothing. A stored
 * time in the future (a clock that went backwards) reads as "just adopted".
 */
export function predecessorMailboxReadable(adoptedAtMs: number | null, nowMs: number): boolean {
  if (adoptedAtMs === null) return true;
  if (!Number.isFinite(adoptedAtMs)) return false;
  return nowMs - adoptedAtMs < PREDECESSOR_MAILBOX_READ_MS;
}

// ---------------------------------------------------------------------------
// The adoption time (rule 4), per account and per predecessor key
// ---------------------------------------------------------------------------

/**
 * One row per account: `{ <predecessor key hex>: <ms> }`. Each predecessor
 * has its OWN 30-day window, counted from the moment that key was left: after
 * a second rotation the first predecessor's window is not started again, and
 * the key just left gets a window of its own.
 */
function adoptionKey(accountIdB64: string): string {
  return `lakko.contacts.predecessor-mailbox-adopted.v2.${accountIdB64}`;
}

/** The storage key of the per-predecessor mailbox cursors of an account
 *  (`{ <predecessor key hex>: <cursor> }`), named here so the wipe and the
 *  poller agree on it. */
export function predecessorCursorStorageKey(accountIdB64: string): string {
  return `lakko.contacts.predecessor-mailbox.v2.${accountIdB64}`;
}

/** The storage key of the adoption times of an account. */
export function predecessorAdoptionStorageKey(accountIdB64: string): string {
  return adoptionKey(accountIdB64);
}

/** Decode a `{ key hex: positive number }` row; anything else reads as
 *  empty. Pure. */
export function decodeNumberByKey(raw: string | null | undefined): Record<string, number> {
  if (raw === null || raw === undefined) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "number" && Number.isFinite(v) && v > 0) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

function readAdoptions(accountIdB64: string): Record<string, number> {
  try {
    return decodeNumberByKey(globalThis.localStorage?.getItem(adoptionKey(accountIdB64)));
  } catch {
    return {};
  }
}

/** When this device left the predecessor key `keyHex` (adopted the keys
 *  that superseded it), or `null` when not stored. */
export function readPredecessorAdoptedAtMs(accountIdB64: string, keyHex: string): number | null {
  return readAdoptions(accountIdB64)[keyHex] ?? null;
}

/**
 * Record when the predecessor key `keyHex` was left. A key is left once: an
 * existing time is kept, so neither a page load nor a LATER rotation extends
 * the window of an earlier predecessor. Never throws.
 */
export function stampPredecessorAdoption(accountIdB64: string, keyHex: string, nowMs: number): void {
  try {
    const held = readAdoptions(accountIdB64);
    if (held[keyHex] !== undefined) return;
    held[keyHex] = Math.floor(nowMs);
    globalThis.localStorage?.setItem(adoptionKey(accountIdB64), JSON.stringify(held));
  } catch {
    /* storage blocked: the mailbox is read for this session */
  }
}

/** Forget the adoption times (both wipes). */
export function clearPredecessorAdoption(accountIdB64: string): void {
  try {
    globalThis.localStorage?.removeItem(adoptionKey(accountIdB64));
  } catch {
    /* nothing was stored either */
  }
}

/**
 * The predecessor mailboxes still to be read (rule 4, over the whole chain):
 * every own predecessor key whose own window is open. `adoptedAtMs` answers
 * a key's stored time (`null` when none: read, and the caller stamps it).
 * Pure.
 */
export function readablePredecessorKeys(
  predecessorKeysHex: readonly string[],
  adoptedAtMs: (keyHex: string) => number | null,
  nowMs: number,
): string[] {
  return predecessorKeysHex.filter((k) => predecessorMailboxReadable(adoptedAtMs(k), nowMs));
}

/**
 * What a PREDECESSOR mailbox may deliver (rules 4 and 5): Welcomes only. A
 * contact request or an ack that reaches a mailbox the account left is not
 * acted on: what it was sealed to, and the reply grant it carries or
 * answers, name keys the holder of the old seed has as well. Pure.
 */
export function predecessorMailboxEvents<E extends { readonly kind: string }>(
  events: readonly E[],
): E[] {
  return events.filter((e) => e.kind === "welcome");
}

// ---------------------------------------------------------------------------
// Runtime: the pin store and the own account
// ---------------------------------------------------------------------------

let ownKeys: AccountKeys | null = null;

/** The signed-in account's own keys (current, and the proven predecessors
 *  this device holds), or `null` when signed out. */
export function setOwnAccountKeys(keys: AccountKeys | null): void {
  ownKeys = keys;
}

export function ownAccountKeys(): AccountKeys | null {
  return ownKeys;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  return out;
}

function bytesOfHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function pinnedNow(): AccountKeys[] {
  try {
    return loadAccountKeys();
  } catch {
    // An unreadable pin store proves nothing: every key names itself.
    return [];
  }
}

/** The account key to address for the person `accountKey` names. */
export function resolveAccountKey(accountKey: Uint8Array): Uint8Array {
  const b64 = base64Encode(accountKey);
  const current = resolveBeforeAddress(b64, pinnedNow(), ownKeys);
  return current === b64 ? accountKey : base64Decode(current);
}

/** {@link resolveAccountKey} over the hex the grant store keys on. */
export function resolveAccountKeyHex(accountKeyHex: string): string {
  try {
    return hexOf(resolveAccountKey(bytesOfHex(accountKeyHex)));
  } catch {
    return accountKeyHex;
  }
}

/** Every key of the person `accountKeyHex` names, as hex: current first. */
export function personKeysHex(accountKeyHex: string): string[] {
  try {
    const b64 = base64Encode(bytesOfHex(accountKeyHex));
    return keysOfPerson(b64, pinnedNow(), ownKeys).map((k) => hexOf(base64Decode(k)));
  } catch {
    return [accountKeyHex];
  }
}
