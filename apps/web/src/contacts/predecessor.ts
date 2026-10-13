// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The proven-predecessor rule (ADR-0074 section 3.5, "Peers"): who is the
 * same person across a key rotation.
 *
 * An account whose published key changed WITH a valid supersession proof (the
 * old key signed the new one) is the same person. The fail-closed gate of
 * ADR-0016 verifies the proof and re-pins silently; from then on the old key
 * is that account's **proven predecessor**. In Stage A the conversation
 * leaves that exist stay bound to the old key, so:
 *
 *   - a leaf bound to a proven predecessor is a leaf of the same person:
 *     names, roles, authorship, read state and the one-to-one peer all
 *     resolve through this table;
 *   - nothing is evicted, no chat closes, nothing is shown;
 *   - a device binding is accepted under the pinned key OR a proven
 *     predecessor of it.
 *
 * A key change WITHOUT a proof (a reset, ADR-0073) never gets this
 * treatment: the old key of a reset is a superseded key, and a later reset
 * of an account makes every predecessor it had, proven or not, superseded.
 * The same rule holds for the account's OWN old key on its own devices.
 *
 * Pure: no store, no clock, no wasm.
 */

/** One account's keys as this device trusts them. */
export interface AccountKeys {
  /** The pinned (for the own account: the held) account key, base64. */
  readonly currentKeyB64: string;
  /** Every key this account had before a PROVEN rotation, oldest first. */
  readonly provenPredecessorsB64: readonly string[];
}

/** predecessor key -> current key, over every account given. */
export type PredecessorTable = ReadonlyMap<string, string>;

/**
 * Build the table. A key that is some account's CURRENT key is never mapped
 * (a current key always names itself), and a predecessor claimed by two
 * accounts is dropped for both: an ambiguous key names nobody.
 */
export function buildPredecessorTable(accounts: readonly AccountKeys[]): PredecessorTable {
  const current = new Set(accounts.map((a) => a.currentKeyB64));
  const out = new Map<string, string>();
  const ambiguous = new Set<string>();
  for (const account of accounts) {
    for (const old of account.provenPredecessorsB64) {
      if (old === account.currentKeyB64 || current.has(old) || ambiguous.has(old)) continue;
      const held = out.get(old);
      if (held !== undefined && held !== account.currentKeyB64) {
        out.delete(old);
        ambiguous.add(old);
        continue;
      }
      out.set(old, account.currentKeyB64);
    }
  }
  return out;
}

/** The current key of the person `keyB64` names: itself, unless it is a
 *  proven predecessor. */
export function currentKeyOf(keyB64: string, table: PredecessorTable): string {
  return table.get(keyB64) ?? keyB64;
}

/** Whether two account keys name the same person. */
export function samePerson(aB64: string, bB64: string, table: PredecessorTable): boolean {
  return currentKeyOf(aB64, table) === currentKeyOf(bB64, table);
}

/** Whether a device binding under `boundKeyB64` belongs to `account`: the
 *  pinned key, or a proven predecessor of it. */
export function bindingAccepted(boundKeyB64: string, account: AccountKeys): boolean {
  return boundKeyB64 === account.currentKeyB64 || account.provenPredecessorsB64.includes(boundKeyB64);
}

/**
 * The key the account's conversation leaves are bound to during Stage A: the
 * OLDEST proven predecessor (the key the leaves were minted under before the
 * first rotation), else the current key. Bindings minted in the interim (a
 * device paired after the rotation, a KeyPackage re-published) use this key,
 * so the conversation layer has one account key everywhere.
 */
export function conversationKeyOf(account: AccountKeys): string {
  return account.provenPredecessorsB64[0] ?? account.currentKeyB64;
}

/** `account` after a PROVEN rotation to `newKeyB64`. A rotation to the key
 *  already held changes nothing. */
export function afterProvenRotation(account: AccountKeys, newKeyB64: string): AccountKeys {
  if (newKeyB64 === account.currentKeyB64) return account;
  const predecessors = account.provenPredecessorsB64.filter((k) => k !== newKeyB64);
  if (!predecessors.includes(account.currentKeyB64)) predecessors.push(account.currentKeyB64);
  return { currentKeyB64: newKeyB64, provenPredecessorsB64: predecessors };
}

/** `account` after an UNPROVEN change (a reset) to `newKeyB64`: no key of
 *  before it is a predecessor any more. */
export function afterReset(_account: AccountKeys, newKeyB64: string): AccountKeys {
  return { currentKeyB64: newKeyB64, provenPredecessorsB64: [] };
}
