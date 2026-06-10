// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// TOTP code-shape validation (ADR-0018 factor (c)).
///
/// Mirror of:
///
///   * `apps/web/src/auth/totp-code.ts`
///   * the server's `parse_code` gate (`services/auth/src/totp.rs`)
///
/// A code is exactly 6 ASCII digits after trimming surrounding
/// whitespace. Validating before the network call saves a PoW solve
/// on `/totp/recover` and lets the forms surface a precise message
/// instead of the server's generic 400/401.

/// Number of digits in a Lakko TOTP code (matches the server's
/// `DIGITS` constant).
public let totpCodeDigits: Int = 6

/// Normalise a user-typed authenticator code: trim surrounding
/// whitespace and accept only exactly ``totpCodeDigits`` ASCII digits
/// (`0-9` — non-ASCII numerals are rejected, same as the server's
/// `is_ascii_digit` check).
///
/// Returns the normalised 6-digit string, or `nil` when the input
/// does not have the right shape.
public func normalizeTotpCode(_ input: String) -> String? {
    let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
    // Count UTF-8 bytes, not Characters: a 6-character string of
    // non-ASCII numerals would pass a Character count but must be
    // rejected. For pure-ASCII input the two counts are equal.
    let bytes = Array(trimmed.utf8)
    guard bytes.count == totpCodeDigits else {
        return nil
    }
    for byte in bytes where byte < 0x30 || byte > 0x39 {
        return nil
    }
    return trimmed
}
