// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Username normalisation and the username character-set rule.
//!
//! Pinned at this layer because plaintext usernames never reach
//! the server — any drift between client and server on what
//! "canonical form" means makes a lookup silently 404 even when
//! the username is registered.
//!
//! The transformation is, in order:
//!
//!   1. Trim whitespace at the edges (so `"\t@alice\n"`
//!      reaches step 2 as `"@alice"`).
//!   2. Strip a single leading `@` if present (so the UI can
//!      accept either `alice` or `@alice`).
//!   3. Trim again (catches `"@ alice"` → `"alice"`).
//!   4. NFKC-normalise (canonical-decompose, then
//!      canonical-compose; collapses compatibility variants, so
//!      fullwidth `ａｌｉｃｅ` becomes `alice`).
//!   5. Lowercase per Unicode `Default_Lowercase`.
//!
//! ## The character-set rule (ADR-0048 §4)
//!
//! [`validate_username`] then requires the normalised form to be
//! ASCII `[a-z0-9_]`, 3 to 32 characters. The rule itself is
//! [`lakko_contacts::username::check_canonical_username`] — one
//! definition shared with the profile decoder, so the name a client
//! registers and the name a client agrees to display can never be
//! judged by two different rules.
//!
//! **This reverses this module's earlier stance.** It used to say
//! that rejecting confusable / homograph characters was "a UI /
//! discovery-policy concern, not a normalisation concern", and that
//! surfacing a "looks like" error here would let an attacker probe
//! the wordspace. Both halves are answered by the new rule:
//!
//!   * It is not a "looks like" check. Nothing compares one name to
//!     another; the alphabet simply has no look-alikes in it. ADR-0048
//!     makes a verified `@username` an identity disambiguator shown
//!     beside a display name, and a displayed `@pavel` that is really
//!     `@pаvel` (Cyrillic `а`) is exactly the impersonation that makes
//!     a username worse than no username. A policy the server cannot
//!     see (it only ever holds a blinded hash) has to be enforced where
//!     every client passes: here, and in the blinder that calls it.
//!   * It reveals nothing about the wordspace. The verdict depends only
//!     on the string typed, never on what is registered.
//!
//! What normalisation still deliberately does NOT do: strip internal
//! whitespace (the rule refuses it instead) or apply IDNA (usernames
//! are not domain names).

use lakko_contacts::username::{check_canonical_username, UsernameError};
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

/// Normalise `input` and require the result to satisfy the ADR-0048 §4
/// username rule — ASCII `[a-z0-9_]`, 3 to 32 characters. Returns the
/// normalised (canonical) form.
///
/// This is what every username entry point must run: signup, search, and
/// — by construction, since the blinder calls it — every OPRF register
/// and lookup.
///
/// # Errors
///
/// A [`UsernameError`] reason code — [`UsernameError::Empty`],
/// [`UsernameError::TooShort`], [`UsernameError::TooLong`] or
/// [`UsernameError::InvalidCharacters`]. None carries the offending
/// character, so an error is safe to log.
pub fn validate_username(input: &str) -> Result<String, UsernameError> {
    let normalised = normalize_username(input);
    check_canonical_username(&normalised)?;
    Ok(normalised)
}

/// The login handle for what the user typed (ADR-0054 S5): normalise,
/// apply the username rule, then
/// [`lakko_contacts::username::login_handle`] —
/// `BLAKE3-derive-key("lakko/auth/login-handle/v1", canonical)`.
///
/// What a client sends `services/auth` to sign in or recover by name on a
/// device that holds no account id. `"@Alice"` and `"alice"` give the same
/// handle.
///
/// # Errors
///
/// The same [`UsernameError`] reasons as [`validate_username`].
pub fn login_handle(
    input: &str,
) -> Result<[u8; lakko_contacts::username::LOGIN_HANDLE_LEN], UsernameError> {
    lakko_contacts::username::login_handle(&validate_username(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_handle_normalises_then_derives() {
        let h = login_handle("  @Alice ").unwrap();
        assert_eq!(h, lakko_contacts::username::login_handle("alice").unwrap());
        assert_eq!(login_handle("al"), Err(UsernameError::TooShort { min: 3 }));
        assert_eq!(
            login_handle("p\u{430}vel"),
            Err(UsernameError::InvalidCharacters)
        );
    }

    #[test]
    fn validate_accepts_and_normalises() {
        assert_eq!(validate_username("alice").unwrap(), "alice");
        assert_eq!(validate_username("Alice").unwrap(), "alice");
        assert_eq!(validate_username("@bob_42").unwrap(), "bob_42");
        assert_eq!(validate_username("  @Bob_42 ").unwrap(), "bob_42");
        // Fullwidth letters fold to ASCII under NFKC, so they are accepted
        // as the ASCII name they normalise to.
        assert_eq!(
            validate_username("\u{ff41}\u{ff4c}\u{ff49}\u{ff43}\u{ff45}").unwrap(),
            "alice"
        );
        assert_eq!(validate_username(&"a".repeat(32)).unwrap(), "a".repeat(32));
        assert_eq!(validate_username("abc").unwrap(), "abc");
    }

    #[test]
    fn validate_rejects_with_typed_reasons() {
        assert_eq!(validate_username(""), Err(UsernameError::Empty));
        assert_eq!(validate_username("@"), Err(UsernameError::Empty));
        assert_eq!(validate_username("   "), Err(UsernameError::Empty));
        assert_eq!(
            validate_username("al"),
            Err(UsernameError::TooShort { min: 3 })
        );
        assert_eq!(
            validate_username(&"a".repeat(33)),
            Err(UsernameError::TooLong { max: 32 })
        );
        for bad in [
            "p\u{430}vel", // Cyrillic а
            "ali-ce",
            "ali.ce",
            "ali ce",
            "alice\u{1F600}",
            "\u{1F600}\u{1F600}\u{1F600}",
            "\u{0430}\u{043d}\u{043d}\u{0430}", // анна
        ] {
            assert_eq!(
                validate_username(bad),
                Err(UsernameError::InvalidCharacters),
                "{bad:?}"
            );
        }
    }

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
