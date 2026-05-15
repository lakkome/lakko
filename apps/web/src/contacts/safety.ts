// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Safety-number computation — TS-side wrapper for Phase 1.9b3's
 * `computeSafetyNumber` WASM export.
 *
 * Surfaces a Signal-style 60-digit ASCII code (12 groups of 5
 * digits separated by single spaces) the user reads OOB against
 * their contact's display to verify they hold the same pinned
 * identity public keys. The wire-level derivation lives in
 * `crates/lakko-contacts::safety` and is order-independent —
 * `computeSafetyNumber(a, b)` and `computeSafetyNumber(b, a)`
 * always produce the same string.
 *
 * The function is async because `loadWasm` is async; the actual
 * BLAKE3 computation is sub-millisecond.
 */
import { loadWasm } from "../wasm";

/** Number of decimal digits in a complete safety number. */
export const SAFETY_NUMBER_DIGITS = 60;

/** Number of 5-digit groups separated by single spaces. */
export const SAFETY_NUMBER_GROUPS = 12;

/**
 * Compute the deterministic safety number for the pair (myPubkey,
 * theirPubkey). Both inputs are 32-byte Ed25519 verifying keys —
 * the same shape every other contact-flow call uses.
 *
 * Returns the 71-byte ASCII string ready for display.
 * `12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890`
 *
 * @throws Error  When either input isn't exactly 32 bytes long
 *                (the WASM surface enforces the size).
 */
export async function computeSafetyNumber(
  myPubkey: Uint8Array,
  theirPubkey: Uint8Array,
): Promise<string> {
  const wasm = await loadWasm();
  return wasm.computeSafetyNumber(myPubkey, theirPubkey);
}
