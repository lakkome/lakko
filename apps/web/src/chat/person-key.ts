// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The key that names a PERSON in a roster (ADR-0050).
 *
 * Since ADR-0050 a conversation leaf is a device, and the same person holds
 * one leaf per device. Everything the UI says about a member — the name, the
 * 1x1 test, roles, authorship, read state — is about the person, so it keys on
 * the leaf's resolved **account key** (`GroupMember.accountKey`), which every
 * device of the account shares. A leaf whose account did not resolve (never
 * admitted by a Lakko gate) falls back to its own key so it still renders.
 */
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";

export interface RosterLeaf {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  readonly accountKey?: Uint8Array;
}

/** The account key of a roster member, else its leaf key. */
export function personKeyOf(m: {
  readonly signaturePublicKey: Uint8Array;
  readonly accountKey?: Uint8Array;
}): Uint8Array {
  return m.accountKey !== undefined && m.accountKey.length === 32
    ? m.accountKey
    : m.signaturePublicKey;
}

/** The distinct people in a roster, base64 account keys in first-leaf order.
 *  A torn group reads as empty. */
export function personKeysOf(group: WasmGroup): string[] {
  let roster: RosterLeaf[];
  try {
    roster = group.members() as unknown as RosterLeaf[];
  } catch {
    return [];
  }
  const out: string[] = [];
  for (const m of roster) {
    const k = base64Encode(personKeyOf(m));
    if (!out.includes(k)) out.push(k);
  }
  return out;
}

/** This device's person key: the account key it is bound to. */
export function ownPersonKeyB64(identity: WasmIdentity | null): string | null {
  if (identity === null) return null;
  return base64Encode(identity.accountPublicKey ?? identity.signaturePublicKey);
}

/** How many PEOPLE a roster holds — what the UI counts and what decides
 *  whether a chat is a 1x1 (two people, whatever their device count). */
export function peopleCountOf(group: WasmGroup): number {
  return personKeysOf(group).length;
}
