// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Is `keyB64` one of the viewer's own keys?
 *
 * A person has two keys a roster or an author line can name them by:
 * the account key every device of theirs is bound to (ADR-0050, what
 * `Group.members()` rows carry as the person key) and the per-device
 * signature key (what a Phase 1 leaf carried, and what a device still
 * signs with). The own profile must answer for both, or the viewer's
 * own row falls through to the received-profile store, which never
 * holds it, and reads `Member`.
 *
 * Pure: no wasm, no storage, no Solid.
 */
import { base64Encode } from "../auth/account-id";

/** The two keys an identity can be named by, base64. */
export interface OwnKeys {
  readonly signaturePublicKey: Uint8Array;
  readonly accountPublicKey?: Uint8Array | null | undefined;
}

export function isOwnKeyB64(own: OwnKeys, keyB64: string): boolean {
  if (base64Encode(own.signaturePublicKey) === keyB64) return true;
  const account = own.accountPublicKey;
  return account !== undefined && account !== null && base64Encode(account) === keyB64;
}
