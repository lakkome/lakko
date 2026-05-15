// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Shamir social-recovery glue (Phase 1.21c).
 *
 * Joins three layers from the WASM core:
 *
 *   1. **`RecoveryMnemonic.toEntropy()`** — extract the 32-byte
 *      BIP39 entropy from a freshly-generated signup phrase
 *      (1.16c3c1) or a user-typed recovery phrase.
 *   2. **`split(entropy, threshold, n)`** + `encodeShare(s)` —
 *      produce N human-typable share strings of the form
 *      `<index>:<base64>`. Default Lakko configuration is
 *      3-of-5 (5 guardians, any 3 reconstruct).
 *   3. **`decodeShare(s)` + `ShareCollector` + `combine()`** —
 *      the inverse path: K strings → reconstructed entropy →
 *      `RecoveryMnemonic.fromEntropy(entropy)` → standard
 *      recovery flow against the auth service.
 *
 * ## Default configuration
 *
 * **3-of-5**: the Lakko-recommended social-recovery shape.
 * Five guardians is the sweet spot for a personal recovery:
 *
 *   * 3 needed to combine — one guardian alone (or two
 *     colluding) cannot reconstruct.
 *   * Tolerates 2 unreachable guardians — death, lost contact,
 *     hostility — without locking the user out.
 *
 * The underlying [`split`] / [`combine`] surface accepts any
 * threshold from 2 up to the share count and any share count
 * up to 16. The UI knobs to expose those past the default land
 * when there's a real user request to surface them.
 */

import { loadWasm } from "../wasm";

/** Default Lakko social-recovery configuration. */
export const DEFAULT_THRESHOLD = 3;
export const DEFAULT_SHARE_COUNT = 5;

/** Plain-data shape mirroring the wasm `Share` handle. */
export interface ShareData {
  readonly index: number;
  readonly data: Uint8Array;
}

/**
 * Run the full split flow: parse the phrase, extract entropy,
 * split into `n` shares with threshold `k`, encode each share
 * to its human-typable wire form.
 *
 * @param phrase    The 24-word BIP39 mnemonic — typically
 *                  obtained from `SignupOutcome.recoveryPhrase`
 *                  or pasted by a user looking to enrol
 *                  social recovery on a recovered account.
 * @param threshold Minimum shares needed to reconstruct
 *                  (default `3`).
 * @param shares    Total shares to produce (default `5`).
 *
 * Returns the encoded shares as strings. The caller drops the
 * `phrase` argument after this call; the returned strings are
 * the only artefact the user takes away.
 *
 * @throws If the phrase fails BIP39 parsing (bad checksum,
 *         wrong word count) or the inner `split` validation
 *         (out-of-range threshold / share count).
 */
export async function splitMnemonic(
  phrase: string,
  threshold: number = DEFAULT_THRESHOLD,
  shares: number = DEFAULT_SHARE_COUNT,
): Promise<string[]> {
  const wasm = await loadWasm();
  const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
  let entropy: Uint8Array;
  try {
    entropy = mnemonic.toEntropy();
  } finally {
    mnemonic.free();
  }

  const result = wasm.split(entropy, threshold, shares);
  // Wipe the entropy buffer in JS land — the wasm `split` has
  // already copied it into the internal polynomial; we don't
  // need the original any more.
  entropy.fill(0);

  try {
    const encoded: string[] = [];
    const count = result.count;
    for (let i = 0; i < count; i++) {
      const share = result.at(i);
      try {
        encoded.push(wasm.encodeShare(share));
      } finally {
        share.free();
      }
    }
    return encoded;
  } finally {
    result.free();
  }
}

/**
 * Run the full combine flow: decode each share string, feed
 * into a `ShareCollector`, combine to the 32-byte entropy,
 * rebuild the mnemonic, return the canonical 24-word phrase.
 *
 * The caller then runs the typed phrase through the standard
 * `AuthContext.recover(phrase, newPassword)` flow — Shamir is
 * a UX layer on top of the existing BIP39 recovery surface;
 * the auth-service round-trip is identical.
 *
 * **Information-theoretic property**: with fewer than the
 * original split's `threshold` shares, `combine` returns
 * garbage entropy. `from_entropy` accepts any 32-byte buffer
 * (BIP39's 8-bit checksum is computed at the constructor, not
 * verified), so this helper returns a syntactically-valid
 * 24-word phrase that just doesn't OPAQUE-authenticate
 * against the server. Indistinguishable from a wrong-share
 * failure by design — the user retries with more / different
 * shares.
 *
 * @throws If any share fails to decode (wrong shape, bad
 *         base64, index 0) or `combine` rejects the input
 *         (duplicate indices, mismatched share lengths, < 2
 *         shares supplied).
 */
export async function combineSharesToMnemonic(
  encodedShares: readonly string[],
): Promise<string> {
  const wasm = await loadWasm();
  const collector = new wasm.ShareCollector();
  let entropy: Uint8Array | null = null;
  try {
    for (const encoded of encodedShares) {
      const share = wasm.decodeShare(encoded.trim());
      try {
        collector.add(share);
      } finally {
        share.free();
      }
    }
    // `combine()` consumes the collector. Stash the entropy +
    // make sure we wipe it before this function returns.
    entropy = collector.combine();
  } catch (err) {
    // `combine()` consumes the collector on success; on the
    // happy path the `try` already drove it to completion. On
    // failure the collector handle is still live + must be
    // freed manually.
    if (entropy === null) {
      try {
        collector.free();
      } catch {
        // collector may have already been consumed by the
        // combine() call that threw; silent.
      }
    }
    throw err;
  }

  const mnemonic = wasm.RecoveryMnemonic.fromEntropy(entropy);
  // Wipe the entropy buffer — we hand back the phrase, not
  // the entropy, and the caller doesn't need it.
  entropy.fill(0);
  try {
    // `phrase` is a wasm-bindgen getter, accessed as a property
    // (no call parens).
    return mnemonic.phrase;
  } finally {
    mnemonic.free();
  }
}

/**
 * Validate a single share string without committing it to a
 * `ShareCollector`. Returns the parsed index on success +
 * `null` on any decoding failure — the recovery UI surfaces
 * "share #3 looks malformed" as the user types each one in.
 */
export async function tryDecodeShare(
  encoded: string,
): Promise<{ index: number } | null> {
  const wasm = await loadWasm();
  try {
    const share = wasm.decodeShare(encoded.trim());
    try {
      return { index: share.index };
    } finally {
      share.free();
    }
  } catch {
    return null;
  }
}
