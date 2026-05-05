// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Username normalisation.
//!
//! Pinned at this layer because plaintext usernames never reach
//! the server — any drift between client and server on what
//! "canonical form" means makes a lookup silently 404 even when
//! the username is registered.
//!
//! The transformation is, in order:
//!
//!   1. Trim ASCII whitespace at the edges (so `"\t@alice\n"`
//!      reaches step 2 as `"@alice"`).
//!   2. Strip a single leading `@` if present (so the UI can
//!      accept either `alice` or `@alice`).
//!   3. Trim again (catches `"@ alice"` → `"alice"`).
//!   4. NFKC-normalise (canonical-decompose, then
//!      canonical-compose; collapses compatibility variants).
//!   5. Lowercase per Unicode `Default_Lowercase`.
//!
//! What we deliberately do NOT do:
//!
//!   * Strip internal whitespace — usernames containing internal
//!     spaces are a UI choice, not a normalisation choice.
//!   * Reject confusable / homograph characters (e.g. `paypal`
//!     vs `paypaI`). That is a UI / discovery-policy concern,
//!     not a normalisation concern, and surfacing a "looks like"
//!     error here would let an attacker probe the wordspace.
//!   * Apply IDNA (Punycode) — usernames are not domain names.

use unicode_normalization::UnicodeNormalization;

/// Map the user-typed string to the canonical form whose OPRF
/// hash binds the username on the server.
///
/// The result is always a fresh `String`; the caller is free to
/// drop the input.
#[must_use]
pub fn normalize_username(input: &str) -> String {
    // Trim → strip @ → trim again. The double-trim handles both
    // "\t@alice\n" (whitespace outside the @) and "@ alice" (a
    // space between the @ and the handle); either is a normal
    // copy-paste artefact.
    let stage1 = input.trim();
    let stage2 = stage1.strip_prefix('@').unwrap_or(stage1).trim();

    // NFKC: K = compatibility, C = composed. Collapses
    // halfwidth / fullwidth / formatted variants that look the
    // same to a reader but encode differently.
    let nfkc: String = stage2.nfkc().collect();

    // Unicode-aware lowercase. `to_lowercase` returns an owned
    // `String` per the `str` API.
    nfkc.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_at() {
        assert_eq!(normalize_username("@alice"), "alice");
        assert_eq!(normalize_username("alice"), "alice");
    }

    #[test]
    fn lowercases_ascii_and_unicode() {
        assert_eq!(normalize_username("Alice"), "alice");
        assert_eq!(normalize_username("ALICE"), "alice");
        // Cyrillic — 'А' (capital) → 'а' (lowercase).
        assert_eq!(normalize_username("Анна"), "анна");
    }

    #[test]
    fn nfkc_collapses_compatibility_forms() {
        // Fullwidth Latin "ｂｏｂ" (U+FF42 …) → ASCII "bob".
        assert_eq!(normalize_username("\u{ff42}\u{ff4f}\u{ff42}"), "bob");
    }

    #[test]
    fn trims_ascii_whitespace_at_edges() {
        assert_eq!(normalize_username("  alice  "), "alice");
        assert_eq!(normalize_username("\t@alice\n"), "alice");
        // The double-trim also catches "@ alice" — a UI/copy-paste
        // artefact where the @ and the handle are space-separated.
        assert_eq!(normalize_username("@ alice"), "alice");
    }

    #[test]
    fn keeps_internal_whitespace() {
        // Not encouraged by the UI, but normalisation does not
        // collapse it. The server stores the resulting hash;
        // future username policy can reject whitespace at the UI
        // layer.
        assert_eq!(normalize_username("foo bar"), "foo bar");
    }

    #[test]
    fn idempotent() {
        let inputs = [
            "@Alice",
            "  BOB  ",
            "\u{ff43}\u{ff45}\u{ff44}\u{ff44}\u{ff59}",
        ];
        for input in inputs {
            let once = normalize_username(input);
            let twice = normalize_username(&once);
            assert_eq!(once, twice, "normalize_username must be idempotent");
        }
    }

    #[test]
    fn empty_inputs() {
        assert_eq!(normalize_username(""), "");
        assert_eq!(normalize_username("   "), "");
        assert_eq!(normalize_username("@"), "");
    }
}
