// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The passkeys this device found cannot carry keys (docs/ux/identity-
 * recovery.md 10.2, amended 2026-11-11): a creation that reported
 * `prf.enabled === false`, or a passkey prompt that gave no PRF output.
 * Their rows read "Logs in only" and do not offer "Restore keys with this
 * passkey" again, across reloads.
 *
 * One `localStorage` row per account (the key carries the account id, so
 * the ADR-0077 store gate in `Storage.prototype.setItem` refuses a late
 * write after Forget device), holding a JSON array of credential ids
 * (base64url). Forget device removes the row (`wipe-account.ts`, step
 * `passkey-no-prf`). Not synced, not sent anywhere: another device may find
 * the same passkey can carry keys there.
 */
import type { MarkerStorage } from "./phrase-gate";

/** At most this many ids are kept; the oldest go first (chosen, not
 *  measured: an account has few passkeys). */
export const NO_PRF_MAX_IDS = 64;

function noPrfKey(accountIdB64: string): string {
  return `lakko:passkey-no-prf:${accountIdB64}`;
}

/** The credential ids recorded for the account; empty when none, the row
 *  is unreadable, or storage is blocked. */
export function readNoPrfPasskeys(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): readonly string[] {
  let raw: string | null;
  try {
    raw = storage?.getItem(noPrfKey(accountIdB64)) ?? null;
  } catch {
    return [];
  }
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

/** Record that `credentialIdB64` cannot carry keys on this device. */
export function addNoPrfPasskey(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  credentialIdB64: string,
): void {
  const ids = readNoPrfPasskeys(storage, accountIdB64).filter((id) => id !== credentialIdB64);
  ids.push(credentialIdB64);
  writeIds(storage, accountIdB64, ids.slice(-NO_PRF_MAX_IDS));
}

/** Forget the record of one passkey (it was removed, or now holds keys). */
export function removeNoPrfPasskey(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  credentialIdB64: string,
): void {
  const before = readNoPrfPasskeys(storage, accountIdB64);
  const after = before.filter((id) => id !== credentialIdB64);
  if (after.length !== before.length) writeIds(storage, accountIdB64, after);
}

/** Remove the account's row (Forget device). */
export function clearNoPrfPasskeys(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(noPrfKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

function writeIds(storage: MarkerStorage | undefined, accountIdB64: string, ids: readonly string[]): void {
  try {
    if (ids.length === 0) storage?.removeItem(noPrfKey(accountIdB64));
    else storage?.setItem(noPrfKey(accountIdB64), JSON.stringify(ids));
  } catch {
    // Storage blocked: the repair is offered again and answers the same.
  }
}
