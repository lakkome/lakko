// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this account owns a group: the one owner check for everything that
 * acts with the owner's key (the knock key, the `@name` handle key).
 *
 * The role table names the owner by the account key it held INSIDE the
 * conversation. After an ADR-0074 rotation that is the conversation key (the
 * predecessor), not the current account key, so both are asked; either one
 * naming the owner slot is this account.
 */
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

/** The keys this account may appear under in a role table. */
export type OwnKeys = readonly (Uint8Array | null | undefined)[];

function sameKey(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

/** Whether the role table's owner is one of `ownKeys`. Pure apart from the
 *  one read of the role table; a failed read is "not the owner". */
export function ownsGroup(group: Pick<WasmGroup, "roleTableOwner">, ownKeys: OwnKeys): boolean {
  let owner: Uint8Array | undefined;
  try {
    owner = group.roleTableOwner() ?? undefined;
  } catch {
    return false;
  }
  if (owner === undefined) return false;
  return ownKeys.some((k) => k !== null && k !== undefined && sameKey(owner, k));
}
