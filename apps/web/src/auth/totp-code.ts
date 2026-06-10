// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TOTP code-shape validation (ADR-0018 factor (c)).
 *
 * Client-side mirror of the server's `parse_code` gate
 * (`services/auth/src/totp.rs`): a code is exactly 6 ASCII digits
 * after trimming surrounding whitespace. Validating before the
 * network call saves a PoW solve on `/totp/recover` and lets the
 * forms surface a precise message instead of the server's generic
 * 400/401.
 */

/** Number of digits in a Lakko TOTP code (matches the server's
 *  `DIGITS` constant). */
export const TOTP_CODE_DIGITS = 6;

/**
 * Normalise a user-typed authenticator code: trim surrounding
 * whitespace and accept only exactly [`TOTP_CODE_DIGITS`] ASCII
 * digits (`0-9` — non-ASCII numerals are rejected, same as the
 * server's `is_ascii_digit` check).
 *
 * Returns the normalised 6-digit string, or `null` when the input
 * does not have the right shape.
 */
export function normalizeTotpCode(input: string): string | null {
  const trimmed = input.trim();
  if (trimmed.length !== TOTP_CODE_DIGITS) {
    return null;
  }
  for (let i = 0; i < trimmed.length; i += 1) {
    const c = trimmed.charCodeAt(i);
    if (c < 0x30 || c > 0x39) {
      return null;
    }
  }
  return trimmed;
}
