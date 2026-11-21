// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Re-publish the account's own identity bundle with its X25519 binding
 * (maintainer decision 2026-11-21).
 *
 * Since the 2026-11-21 security review every client seals to an account
 * X25519 key only when the directory serves `x25519_binding` beside it
 * (`discovery/x25519-binding.ts`), and discovery requires the binding on
 * every upload. Rows written before discovery migration 0041 carry none, and
 * a reload does not upload the bundle when the directory already serves this
 * device's keys, so an account that last uploaded before the fix could not be
 * sent a contact request until it signed in again.
 *
 * The rule runs in the foreground landing (`refreshPublishedIdentity` in
 * `AuthContext.tsx`), on the bundle that landing already read for the
 * device's OWN account, never from the service worker:
 *
 *   - the row serves this device's signature key and X25519 key and a binding
 *     that verifies: the binding is confirmed, nothing is uploaded;
 *   - the row serves this device's keys with no binding, or one that does
 *     not verify: the same keys are uploaded again, now with the binding,
 *     through the landing's own upload (a same-key upload; discovery keeps
 *     the stored proof, leaf authorisation, genesis attestation, generation
 *     and conversation link, `COALESCE` in `services/discovery/src/
 *     identity_bundle.rs`);
 *   - no row, or a row with other keys: nothing here (the landing rule and
 *     the key-reset logic own those cases).
 *
 * A confirmed binding is remembered per account in plain storage, keyed on
 * the account key it was confirmed for (a key reset elsewhere makes the next
 * landing check again). The row's name carries the account id, so the
 * ADR-0077 store gate refuses a late write after Forget device, which removes
 * it (`wipe-account.ts`, step `x25519-binding-confirmed`). A failure leaves
 * the mark unset; the next landing tries again.
 */
import type { MarkerStorage } from "./phrase-gate";
import {
  boundX25519With,
  type BindingFields,
  type X25519BindingVerifier,
} from "../discovery/x25519-binding";

function markKey(accountIdB64: string): string {
  return `lakko:x25519-binding-confirmed:${accountIdB64}`;
}

/** The account key (standard base64) the binding was last confirmed for on
 *  this browser, or `null`. */
export function readBindingConfirmed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): string | null {
  try {
    return storage?.getItem(markKey(accountIdB64)) ?? null;
  } catch {
    return null;
  }
}

export function writeBindingConfirmed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  accountKeyB64: string,
): void {
  try {
    storage?.setItem(markKey(accountIdB64), accountKeyB64);
  } catch {
    // Storage blocked: the check runs again at the next landing.
  }
}

export function clearBindingConfirmed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(markKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

/** This device's own account keys. */
export interface OwnAccountKeys {
  readonly signaturePublicKey: Uint8Array;
  readonly x25519PublicKey: Uint8Array;
}

/** What the directory's own row means for the binding. */
export type OwnBindingCheck =
  /** The row serves this device's keys and a binding that verifies. */
  | "confirmed"
  /** The row serves this device's keys without a binding that verifies. */
  | "republish"
  /** No row, or a row with other keys: not this rule's case. */
  | "not-own";

function equalBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

function decodeStd(b64: string): Uint8Array | null {
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function encodeStd(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** The rule on the fetched row. Pure: the verifier is passed in. */
export function ownBindingCheck(
  row: BindingFields | null,
  own: OwnAccountKeys,
  verify: X25519BindingVerifier,
): OwnBindingCheck {
  if (row === null) return "not-own";
  const signature = decodeStd(row.signature_pubkey);
  const x25519 = decodeStd(row.x25519_pubkey);
  if (signature === null || x25519 === null) return "not-own";
  if (!equalBytes(signature, own.signaturePublicKey) || !equalBytes(x25519, own.x25519PublicKey)) {
    return "not-own";
  }
  return boundX25519With(verify, row) === null ? "republish" : "confirmed";
}

/** What one run did. */
export type BindingRepublishOutcome =
  /** The mark was already set for this key; nothing was checked. */
  | "already-confirmed"
  /** The row already carried a valid binding; the mark is now set. */
  | "confirmed"
  /** The keys were uploaded again with the binding; the mark is now set. */
  | "republished"
  /** No row, or a row with other keys; nothing was done. */
  | "not-own"
  /** The upload failed; the mark stays unset. */
  | "failed";

export interface BindingRepublishDeps {
  readonly storage: MarkerStorage | undefined;
  readonly accountIdB64: string;
  readonly own: OwnAccountKeys;
  /** The device's own row as the landing read it (`null` for a 404). */
  readonly row: BindingFields | null;
  readonly verify: X25519BindingVerifier;
  /** The landing's own upload of this device's keys (it signs the binding). */
  readonly upload: () => Promise<void>;
  /** One plain line on failure; carries no identifier. */
  readonly warn?: (line: string) => void;
}

/**
 * Run the rule once. Never throws; a failure leaves the mark unset so the
 * next landing tries again.
 */
export async function republishBindingIfMissing(
  deps: BindingRepublishDeps,
): Promise<BindingRepublishOutcome> {
  const ownKeyB64 = encodeStd(deps.own.signaturePublicKey);
  if (readBindingConfirmed(deps.storage, deps.accountIdB64) === ownKeyB64) {
    return "already-confirmed";
  }
  const check = ownBindingCheck(deps.row, deps.own, deps.verify);
  if (check === "not-own") return "not-own";
  if (check === "confirmed") {
    writeBindingConfirmed(deps.storage, deps.accountIdB64, ownKeyB64);
    return "confirmed";
  }
  try {
    await deps.upload();
  } catch (err) {
    const reason = err instanceof Error ? err.name : "unknown";
    (deps.warn ?? defaultWarn)(`identity binding re-publish failed (${reason})`);
    return "failed";
  }
  writeBindingConfirmed(deps.storage, deps.accountIdB64, ownKeyB64);
  return "republished";
}

function defaultWarn(line: string): void {
  // eslint-disable-next-line no-console
  console.warn(line);
}
