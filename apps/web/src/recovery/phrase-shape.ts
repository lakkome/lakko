// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shape of the recovery phrase (ADR-0072 decision 3): 12 BIP39 English
 * words, 128 bits of entropy. The authority is `lakko-mnemonic`
 * (`WORD_COUNT`, `ENTROPY_LEN`), which the wasm surface does not export as
 * constants; `phrase-shape.test.ts` pins these two against what the wasm
 * core generates, so a change in the core fails here instead of drifting.
 *
 * Pure (no wasm, no Solid), so the UI and the tests can both import it.
 */

/** Words in a recovery phrase. */
export const RECOVERY_PHRASE_WORDS = 12;

/** Bytes of entropy behind a recovery phrase. */
export const RECOVERY_PHRASE_ENTROPY_BYTES = 16;
