// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * TOTP code-shape validation (ADR-0018 factor (c)).
 *
 * Client-side mirror of the server's `parse_code` gate
 * (`services/auth/src/totp.rs`) and of the web
 * `apps/web/src/auth/totp-code.ts`: a code is exactly 6 ASCII
 * digits after trimming surrounding whitespace. Validating before
 * the network call saves a PoW solve on `/totp/recover` and lets
 * the forms surface a precise message instead of the server's
 * generic 400/401.
 */

/** Number of digits in a Lakko TOTP code (matches the server's
 *  `DIGITS` constant in `services/auth/src/totp.rs`). */
public const val TOTP_CODE_DIGITS: Int = 6

/**
 * Normalise a user-typed authenticator code: trim surrounding
 * whitespace and accept only exactly [TOTP_CODE_DIGITS] ASCII
 * digits (`0-9` — non-ASCII numerals are rejected, same as the
 * server's `is_ascii_digit` check; Kotlin's `Char.isDigit()`
 * would wrongly accept e.g. Arabic-Indic digits, so the range
 * comparison is explicit).
 *
 * Returns the normalised 6-digit string, or `null` when the
 * input does not have the right shape.
 */
public fun normalizeTotpCode(input: String): String? {
    val trimmed = input.trim()
    if (trimmed.length != TOTP_CODE_DIGITS) {
        return null
    }
    for (c in trimmed) {
        if (c < '0' || c > '9') {
            return null
        }
    }
    return trimmed
}
