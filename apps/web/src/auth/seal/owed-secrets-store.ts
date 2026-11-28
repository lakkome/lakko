// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The secrets an owed step of `account-seal-jobs.ts` still needs, kept so a
 * reload or a crash does not lose the step (ADR-0101 sections 7 and 10):
 *
 * - `reset`: what sealed a key reset made on this device (the export key, or
 *   a passkey's `prf` output and tag) and the export key of the password in
 *   force, until the owned-channels retake answered. Without it a reload
 *   between the reset and the retake left a passkey-sealed retake with
 *   nothing to open the list with.
 * - `passkeyLocks`: a passkey that keeps the chats whose seal or whose
 *   owned-channels lock could not be written yet.
 * - `passkeyLockRemovals`: a deleted passkey whose lock is still to go.
 * - `passwordLock`: the export key of a password event of this device whose
 *   owned-channels password lock was not written yet. It is used again only
 *   while the account's announcement lists that password (a password changed
 *   elsewhere meanwhile drops it), and never to announce anything.
 *
 * One row per account, `lakko:owned-owed-secrets:<account>`, sealed under
 * the account's device key: `iv(12) ‖ AES-256-GCM(JSON)` in standard base64.
 * Deleted when nothing is owed, and by Forget device. Never logged.
 */
import type { DeviceCipher } from "../device-cipher";
import type { MarkerStorage } from "../phrase-gate";

export interface OwedResetSecrets {
  readonly sealedBy:
    | { readonly kind: "password"; readonly exportKey: Uint8Array }
    | { readonly kind: "passkey"; readonly prfOutput: Uint8Array; readonly credentialTag: Uint8Array }
    | null;
  readonly exportKey: Uint8Array | null;
  /** A recall: the root of the old keys lock (32 bytes), else `null`. */
  readonly oldKeysRoot: Uint8Array | null;
}

export interface OwedPasskeyLock {
  /** The passkey's credential id (its seal tag is derived from it). */
  readonly credentialId: Uint8Array;
  readonly prfOutput: Uint8Array;
  /** Whether its seal entry is published already (only the lock is owed). */
  readonly sealed: boolean;
}

export interface OwedSecrets {
  readonly reset: OwedResetSecrets | null;
  readonly passkeyLocks: readonly OwedPasskeyLock[];
  /** Lower-case hex tags. */
  readonly passkeyLockRemovals: readonly string[];
  readonly passwordLock: Uint8Array | null;
}

export const NOTHING_OWED: OwedSecrets = { reset: null, passkeyLocks: [], passkeyLockRemovals: [], passwordLock: null };

function rowKey(accountIdB64: string): string {
  return `lakko:owned-owed-secrets:${accountIdB64}`;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function unb64(text: unknown): Uint8Array | null {
  if (typeof text !== "string") return null;
  try {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** Whether nothing is owed. Pure. */
export function nothingOwed(o: OwedSecrets): boolean {
  return o.reset === null && o.passkeyLocks.length === 0 && o.passkeyLockRemovals.length === 0 && o.passwordLock === null;
}

function encode(o: OwedSecrets): string {
  const reset =
    o.reset === null
      ? null
      : {
          sealedBy:
            o.reset.sealedBy === null
              ? null
              : o.reset.sealedBy.kind === "password"
                ? { kind: "password", exportKey: b64(o.reset.sealedBy.exportKey) }
                : {
                    kind: "passkey",
                    prfOutput: b64(o.reset.sealedBy.prfOutput),
                    credentialTag: b64(o.reset.sealedBy.credentialTag),
                  },
          exportKey: o.reset.exportKey === null ? null : b64(o.reset.exportKey),
          oldKeysRoot: o.reset.oldKeysRoot === null ? null : b64(o.reset.oldKeysRoot),
        };
  return JSON.stringify({
    v: 1,
    reset,
    passkeyLocks: o.passkeyLocks.map((p) => ({
      credentialId: b64(p.credentialId),
      prfOutput: b64(p.prfOutput),
      sealed: p.sealed,
    })),
    passkeyLockRemovals: o.passkeyLockRemovals,
    passwordLock: o.passwordLock === null ? null : b64(o.passwordLock),
  });
}

/** The row's content; anything malformed reads as nothing owed. Pure. */
export function decodeOwedSecrets(text: string): OwedSecrets {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return NOTHING_OWED;
  }
  if (raw === null || typeof raw !== "object" || (raw as { v?: unknown }).v !== 1) return NOTHING_OWED;
  const r = raw as Record<string, unknown>;
  let reset: OwedResetSecrets | null = null;
  if (r.reset !== null && typeof r.reset === "object") {
    const rr = r.reset as Record<string, unknown>;
    let sealedBy: OwedResetSecrets["sealedBy"] = null;
    const sb = rr.sealedBy as Record<string, unknown> | null | undefined;
    if (sb !== null && typeof sb === "object") {
      if (sb.kind === "password") {
        const k = unb64(sb.exportKey);
        if (k !== null && k.length === 64) sealedBy = { kind: "password", exportKey: k };
      } else if (sb.kind === "passkey") {
        const prf = unb64(sb.prfOutput);
        const tag = unb64(sb.credentialTag);
        if (prf !== null && prf.length === 32 && tag !== null) sealedBy = { kind: "passkey", prfOutput: prf, credentialTag: tag };
      }
    }
    const k = rr.exportKey === null ? null : unb64(rr.exportKey);
    const root = rr.oldKeysRoot === null || rr.oldKeysRoot === undefined ? null : unb64(rr.oldKeysRoot);
    reset = {
      sealedBy,
      exportKey: k !== null && k.length === 64 ? k : null,
      oldKeysRoot: root !== null && root.length === 32 ? root : null,
    };
  }
  const passkeyLocks: OwedPasskeyLock[] = [];
  if (Array.isArray(r.passkeyLocks)) {
    for (const p of r.passkeyLocks as Record<string, unknown>[]) {
      const id = unb64(p?.credentialId);
      const prf = unb64(p?.prfOutput);
      if (id !== null && id.length > 0 && prf !== null && prf.length === 32) {
        passkeyLocks.push({ credentialId: id, prfOutput: prf, sealed: p.sealed === true });
      }
    }
  }
  const passkeyLockRemovals = Array.isArray(r.passkeyLockRemovals)
    ? (r.passkeyLockRemovals as unknown[]).filter((t): t is string => typeof t === "string" && /^[0-9a-f]{32}$/.test(t))
    : [];
  const pl = r.passwordLock === null ? null : unb64(r.passwordLock);
  return { reset, passkeyLocks, passkeyLockRemovals, passwordLock: pl !== null && pl.length === 64 ? pl : null };
}

/** What is owed for the account; nothing when no row is kept or it does not
 *  open under this device key. */
export async function readOwedSecrets(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<OwedSecrets> {
  try {
    const raw = storage?.getItem(rowKey(accountIdB64)) ?? null;
    if (raw === null) return NOTHING_OWED;
    const row = unb64(raw);
    if (row === null || row.length <= 12) return NOTHING_OWED;
    const plain = new Uint8Array(await deviceKey.decrypt(row.slice(0, 12), row.slice(12)));
    return decodeOwedSecrets(new TextDecoder().decode(plain));
  } catch {
    return NOTHING_OWED;
  }
}

/** Write what is owed (the row goes when nothing is). `false` when it could
 *  not be written (storage blocked). */
export async function writeOwedSecrets(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceKey: DeviceCipher,
  owed: OwedSecrets,
): Promise<boolean> {
  if (storage === undefined) return false;
  try {
    if (nothingOwed(owed)) {
      storage.removeItem(rowKey(accountIdB64));
      return true;
    }
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = new Uint8Array(await deviceKey.encrypt(iv, new TextEncoder().encode(encode(owed))));
    const row = new Uint8Array(iv.length + ct.length);
    row.set(iv, 0);
    row.set(ct, iv.length);
    storage.setItem(rowKey(accountIdB64), b64(row));
    return true;
  } catch {
    return false;
  }
}

/** Forget device: the row goes. */
export function clearOwedSecrets(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(rowKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
