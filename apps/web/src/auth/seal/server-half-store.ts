// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owned-channels server half `S` while it is still owed (ADR-0101
 * section 7): auth releases it only in the answer of `/signup/finish` and
 * against a reset factor grant, so the device that received it must not lose
 * it before the vaults are made. It is kept here, sealed under the account's
 * device key, from the moment it arrives until `makeOwnedChannelsVaults`
 * answered `"made"` (a guest keeps it until its claim sets the first
 * password). After that the keys lock carries it and this row is deleted.
 *
 * The row is `iv(12) ‖ AES-256-GCM(S)` in standard base64. Never logged.
 */
import type { DeviceCipher } from "../device-cipher";
import type { MarkerStorage } from "../phrase-gate";

function rowKey(accountIdB64: string): string {
  return `lakko:owned-server-half:${accountIdB64}`;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function unb64(text: string): Uint8Array {
  const raw = atob(text);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
  return out;
}

/** Keep `serverHalf` (32 bytes) for the account. `false` when it could not
 *  be written (storage blocked): the caller still holds it in memory. */
export async function keepServerHalf(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  serverHalf: Uint8Array,
  deviceKey: DeviceCipher,
): Promise<boolean> {
  if (storage === undefined || serverHalf.length !== 32) return false;
  try {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = new Uint8Array(await deviceKey.encrypt(iv, serverHalf));
    const row = new Uint8Array(iv.length + ct.length);
    row.set(iv, 0);
    row.set(ct, iv.length);
    storage.setItem(rowKey(accountIdB64), b64(row));
    return true;
  } catch {
    return false;
  }
}

/** The kept server half, or `null` when none is kept or the row does not
 *  open under this device key. */
export async function keptServerHalf(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  try {
    const raw = storage?.getItem(rowKey(accountIdB64)) ?? null;
    if (raw === null) return null;
    const row = unb64(raw);
    if (row.length <= 12) return null;
    const plain = new Uint8Array(await deviceKey.decrypt(row.slice(0, 12), row.slice(12)));
    return plain.length === 32 ? plain : null;
  } catch {
    return null;
  }
}

/** The vaults are made (or the account is wiped): the row goes. */
export function clearServerHalf(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(rowKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
