// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The canonical username character-set rule (ADR-0048 §4).
//!
//! ## The rule
//!
//! A username, **in its normalised form**, is ASCII lowercase letters,
//! digits and underscore only — `[a-z0-9_]` — and 3 to 32 characters
//! long. Because the alphabet is ASCII, "characters" and "bytes" are the
//! same count here, so there is no grapheme or scalar-value ambiguity to
//! argue about.
//!
//! The rule is the maintainer's (2026-09-13): Cyrillic and Latin
//! look-alikes (`pavel` vs `pаvel`, where the second `а` is U+0430) and
//! dashes or dots cannot be used to make one username read as another,
//! because none of those characters is admissible at all.
//!
//! ## Why this lives here and not in `lakko-discovery-client`
//!
//! Two crates need exactly this check and must never disagree about it:
//!
//! * `lakko-discovery-client`, whose OPRF blinder refuses to blind a
//!   name that fails it (so no conforming client can register or look up
//!   a non-conforming name);
//! * [`crate::profile`], whose decoder refuses a profile envelope whose
//!   claimed username fails it (so no conforming client can display one).
//!
//! Normalisation (NFKC, lowercase, strip `@`) needs Unicode tables and
//! stays in `lakko-discovery-client`, which already carries them. **This
//! check needs no tables at all** — it only accepts bytes that are
//! already canonical — so it can live in the crate with the smaller
//! dependency graph and be depended on from the other side.
//! `lakko-contacts` depends only on `thiserror`, `tracing` and
//! `lakko-crypto`; putting the check in `lakko-discovery-client` instead
//! would have dragged `voprf`, `unicode-normalization`, `rand` and
//! `base64` into every `lakko-contacts` consumer, including the
//! size-budgeted iOS NotificationServiceExtension (`lakko-ffi-extension`),
//! which links `lakko-contacts` to classify service envelopes.
//!
//! The dependency this adds instead — `lakko-discovery-client` ->
//! `lakko-contacts` — adds no third-party crate to any shipped artefact:
//! both FFI crates that link `lakko-discovery-client` already link
//! `lakko-contacts`.
//!
//! ## What this does NOT do
//!
//! It does not normalise. `"Alice"` and `"@alice"` fail here, because they
//! are not canonical. A caller holding user input runs
//! `lakko_discovery_client::validate_username`, which normalises first
//! and then applies this rule.
//!
//! ## Errors never echo the input
//!
//! [`UsernameError`] is a reason code. None of its variants carries the
//! offending character or the rejected string, so logging one cannot leak
//! a lookup target (a username a user searched for is exactly the thing
//! the OPRF exists to hide from the server).

use thiserror::Error;

/// Shortest admissible username, in characters (= bytes; the alphabet is
/// ASCII).
pub const USERNAME_MIN_LEN: usize = 3;

/// Longest admissible username, in characters (= bytes; the alphabet is
/// ASCII).
pub const USERNAME_MAX_LEN: usize = 32;

/// Why a username was refused.
///
/// A reason code, deliberately free of the input: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum UsernameError {
    /// Nothing left once normalised (e.g. `""`, `"   "`, `"@"`).
    #[error("username is empty")]
    Empty,
    /// Shorter than [`USERNAME_MIN_LEN`].
    #[error("username is shorter than {min} characters")]
    TooShort {
        /// Mirror of [`USERNAME_MIN_LEN`], so the message is self-contained.
        min: usize,
    },
    /// Longer than [`USERNAME_MAX_LEN`].
    #[error("username is longer than {max} characters")]
    TooLong {
        /// Mirror of [`USERNAME_MAX_LEN`], so the message is self-contained.
        max: usize,
    },
    /// Contains a character outside `[a-z0-9_]`. The character itself is
    /// deliberately not reported.
    #[error("username may contain only Latin letters, digits and underscore")]
    InvalidCharacters,
}

impl UsernameError {
    /// Stable, language-neutral reason code — what an FFI surface hands a
    /// client to select its copy from, instead of the English message.
    ///
    /// Frozen strings: a client matches on them.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooShort { .. } => "tooShort",
            Self::TooLong { .. } => "tooLong",
            Self::InvalidCharacters => "invalidCharacters",
        }
    }
}

/// `true` for exactly the bytes admissible in a canonical username.
const fn is_username_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_')
}

/// Check that `candidate` is **already** a canonical username.
///
/// The reasons are tested in a fixed order — empty, then characters,
/// then length — so a short string of forbidden characters reports the
/// character problem, which is the one the user has to fix first
/// (`"а-"` is `InvalidCharacters`, not `TooShort`). Length is only
/// meaningful once every character is ASCII, since only then does a
/// byte count equal a character count.
///
/// # Errors
///
/// Returns the first [`UsernameError`] that applies.
pub fn check_canonical_username(candidate: &str) -> Result<(), UsernameError> {
    if candidate.is_empty() {
        return Err(UsernameError::Empty);
    }
    if !candidate.bytes().all(is_username_byte) {
        return Err(UsernameError::InvalidCharacters);
    }
    // All bytes are ASCII from here on, so `len()` is the character count.
    if candidate.len() < USERNAME_MIN_LEN {
        return Err(UsernameError::TooShort {
            min: USERNAME_MIN_LEN,
        });
    }
    if candidate.len() > USERNAME_MAX_LEN {
        return Err(UsernameError::TooLong {
            max: USERNAME_MAX_LEN,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() {
        for ok in ["alice", "bob_42", "abc", "a_b", "___", "0123456789"] {
            assert_eq!(check_canonical_username(ok), Ok(()), "{ok}");
        }
        let longest = "a".repeat(USERNAME_MAX_LEN);
        assert_eq!(check_canonical_username(&longest), Ok(()));
    }

    #[test]
    fn length_boundaries_discriminate() {
        assert_eq!(
            check_canonical_username("ab"),
            Err(UsernameError::TooShort { min: 3 })
        );
        assert_eq!(check_canonical_username("abc"), Ok(()));
        assert_eq!(check_canonical_username(&"a".repeat(32)), Ok(()));
        assert_eq!(
            check_canonical_username(&"a".repeat(33)),
            Err(UsernameError::TooLong { max: 32 })
        );
    }

    #[test]
    fn refuses_non_canonical_characters() {
        for bad in [
            "Alice",       // uppercase is not canonical — normalise first
            "@alice",      // so is a leading @
            "p\u{430}vel", // Cyrillic а
            "ali-ce",
            "ali.ce",
            "ali ce",
            "alice\u{1F600}",
            "al\u{0}ice",
            "\u{ff41}lice", // fullwidth a — NFKC would fold it, this check does not
        ] {
            assert_eq!(
                check_canonical_username(bad),
                Err(UsernameError::InvalidCharacters),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn empty_is_its_own_reason() {
        assert_eq!(check_canonical_username(""), Err(UsernameError::Empty));
    }

    #[test]
    fn characters_are_reported_before_length() {
        // Two forbidden characters: the user must fix the alphabet first.
        assert_eq!(
            check_canonical_username("\u{430}-"),
            Err(UsernameError::InvalidCharacters)
        );
        // A long run of forbidden multi-byte characters is not "too long".
        assert_eq!(
            check_canonical_username(&"\u{430}".repeat(40)),
            Err(UsernameError::InvalidCharacters)
        );
    }

    #[test]
    fn error_messages_never_echo_the_input() {
        let secret_target = "p\u{430}vel_secret";
        let err = check_canonical_username(secret_target).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("pavel") && !msg.contains('\u{430}') && !msg.contains("secret"));
        assert!(!format!("{err:?}").contains('\u{430}'));
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(UsernameError::Empty.code(), "empty");
        assert_eq!(UsernameError::TooShort { min: 3 }.code(), "tooShort");
        assert_eq!(UsernameError::TooLong { max: 32 }.code(), "tooLong");
        assert_eq!(UsernameError::InvalidCharacters.code(), "invalidCharacters");
    }
}
