// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 6 with ADR-0074 — which seed a blob's delete key is
 * derived from.
 *
 * Not the current account identity: a key rotation (ADR-0074) replaces it,
 * and every file uploaded before the rotation would then carry a delete key
 * no device can sign for. The account's LINEAGE seed (the oldest own
 * predecessor after a rotation, else the account seed; the seed the
 * DeviceGroup id comes from, `devices/lifecycle.ts`) does not move, so the
 * delete key of a blob is the same before and after. Only the blob delete
 * keys use it; the ADR-0064 vault ids and keys are unchanged.
 */

/** The wasm surface this needs, structurally. */
export interface BlobDeleteKeyWasm<I, K> {
  readonly Identity: { fromSeed(seed: Uint8Array): I };
  vaultKeys(identity: I): K;
}

/**
 * The `VaultKeys` blob delete keys are made with: from `lineageSeed` when the
 * device holds one, else from `current` (the account identity, used when no
 * seed is readable here at all).
 */
export function blobDeleteVaultKeys<I extends { free?(): void }, K>(
  wasm: BlobDeleteKeyWasm<I, K>,
  lineageSeed: Uint8Array | null,
  current: I,
): K {
  if (lineageSeed === null) return wasm.vaultKeys(current);
  const lineage = wasm.Identity.fromSeed(lineageSeed);
  try {
    return wasm.vaultKeys(lineage);
  } finally {
    try {
      lineage.free?.();
    } catch {
      /* already freed */
    }
  }
}
