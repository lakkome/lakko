// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Light client-side contact-shape validation (ADR-0018 factor (d)).
///
/// Mirror of:
///
///   * `apps/web/src/auth/contact-shape.ts` (`isPlausibleContact`)
///
/// A PERMISSIVE mirror of the server's `normalize_contact` gate
/// (`services/auth/src/recovery_contact.rs`): just enough of a check
/// to catch an obviously-malformed contact (e.g. an email with no
/// `@`, a phone with no leading `+`) before spending a PoW solve and
/// a round-trip. The server is authoritative — it does the full
/// E.164 / RFC-ish normalization and rejects with a 400 — so this
/// deliberately stays loose and never blocks a borderline value the
/// server might accept.

/// Return `true` when `contact` is plausibly shaped for `kind`:
///
///   - **email** (``ContactKind/email``) — contains exactly one `@`
///     with a non-empty local part and domain, no whitespace, and is
///     at most 254 characters.
///   - **phone** (``ContactKind/phone``) — after stripping the common
///     human separators (spaces, dashes, parentheses), starts with
///     `+` followed by 1..=15 digits (digits only thereafter).
///
/// Whitespace is trimmed first. Anything else returns `false` so the
/// form can surface a precise message instead of burning a PoW solve.
public func isPlausibleContact(kind: ContactKind, contact: String) -> Bool {
    let trimmed = contact.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty {
        return false
    }
    switch kind {
    case .email:
        // No internal whitespace allowed.
        if trimmed.rangeOfCharacter(from: .whitespacesAndNewlines) != nil {
            return false
        }
        guard let at = trimmed.firstIndex(of: "@"),
              trimmed.lastIndex(of: "@") == at else {
            // Zero or more than one '@'.
            return false
        }
        // Non-empty local part and domain.
        let localNonEmpty = at != trimmed.startIndex
        let domainNonEmpty = trimmed.index(after: at) != trimmed.endIndex
        return localNonEmpty && domainNonEmpty && trimmed.count <= 254
    case .phone:
        // Strip the separators the server also strips, then require a
        // leading '+' and 1..=15 digits.
        let separators: Set<Character> = [" ", "-", "(", ")", "\u{00A0}"]
        let stripped = String(trimmed.filter { !separators.contains($0) })
        guard stripped.first == "+" else {
            return false
        }
        let digits = stripped.dropFirst()
        return digits.count >= 1
            && digits.count <= 15
            && digits.allSatisfy { $0.isASCII && $0.isNumber }
    }
}
