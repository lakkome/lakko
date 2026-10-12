// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The recovery email address rule (ADR-0067 section 4), written once.
//!
//! [`normalize_recovery_email`] is the one definition of what a recovery
//! address is and how it is spelled before anything is derived from it. The
//! client calls it before sealing or opening the email vault
//! ([`crate::vault`]) and `services/auth` calls it before computing its
//! keyed hash of the address, so the two cannot disagree on a byte.
//!
//! The rule is deliberately small: trim, lower-case the whole address,
//! require exactly one `@` with something on both sides, 3 to 254 bytes, no
//! control characters and no white space. There are no provider-specific
//! rules (no dot or plus-suffix folding) and no attempt to validate the
//! domain: whether the address exists is established by the mailed code.
//!
//! The function is pure string handling: it runs no key derivation and
//! touches no secret.

use thiserror::Error;

/// Shortest normalised address, in bytes (`a@b`).
pub const RECOVERY_EMAIL_MIN_LEN: usize = 3;

/// Longest normalised address, in bytes (the RFC 5321 path limit).
pub const RECOVERY_EMAIL_MAX_LEN: usize = 254;

/// Why an address was refused. No variant carries the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RecoveryEmailError {
    /// Shorter than [`RECOVERY_EMAIL_MIN_LEN`] bytes after normalisation.
    #[error("the address is too short")]
    TooShort,
    /// Longer than [`RECOVERY_EMAIL_MAX_LEN`] bytes after normalisation.
    #[error("the address is too long")]
    TooLong,
    /// The address holds a control character.
    #[error("the address holds a control character")]
    ControlCharacter,
    /// The address holds white space inside it.
    #[error("the address holds white space")]
    Whitespace,
    /// The address does not hold exactly one `@`.
    #[error("the address must hold exactly one @")]
    NotOneAtSign,
    /// Nothing before or nothing after the `@`.
    #[error("the address needs text on both sides of the @")]
    EmptyPart,
}

impl RecoveryEmailError {
    /// A stable kebab-case code for this refusal. Frozen: clients branch on
    /// it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooShort => "too-short",
            Self::TooLong => "too-long",
            Self::ControlCharacter => "control-character",
            Self::Whitespace => "whitespace",
            Self::NotOneAtSign => "not-one-at-sign",
            Self::EmptyPart => "empty-part",
        }
    }
}

/// Normalise a recovery email address: trim surrounding white space and
/// lower-case the whole address, then check it.
///
/// The result is what every derivation takes (the email vault key, the
/// server's `email_key`). Normalising an already normalised address returns
/// it unchanged.
///
/// # Errors
///
/// A [`RecoveryEmailError`] when the normalised address holds a control
/// character or white space, is not 3 to 254 bytes long, does not hold
/// exactly one `@`, or has nothing on one side of it. The checks run in
/// that order.
pub fn normalize_recovery_email(address: &str) -> Result<String, RecoveryEmailError> {
    let normalised = address.trim().to_lowercase();
    if normalised.chars().any(char::is_control) {
        return Err(RecoveryEmailError::ControlCharacter);
    }
    if normalised.chars().any(char::is_whitespace) {
        return Err(RecoveryEmailError::Whitespace);
    }
    if normalised.len() < RECOVERY_EMAIL_MIN_LEN {
        return Err(RecoveryEmailError::TooShort);
    }
    if normalised.len() > RECOVERY_EMAIL_MAX_LEN {
        return Err(RecoveryEmailError::TooLong);
    }
    let mut parts = normalised.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(RecoveryEmailError::NotOneAtSign);
    };
    if local.is_empty() || domain.is_empty() {
        return Err(RecoveryEmailError::EmptyPart);
    }
    Ok(normalised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lower_cases_the_whole_address() {
        assert_eq!(
            normalize_recovery_email("  Alice.Smith+Tag@Example.COM \n").unwrap(),
            "alice.smith+tag@example.com"
        );
        // No provider rules: dots and plus suffixes are kept.
        assert_eq!(
            normalize_recovery_email("a.b+c@gmail.com").unwrap(),
            "a.b+c@gmail.com"
        );
        // Non-ASCII letters are lower-cased too.
        assert_eq!(
            normalize_recovery_email("ÄNNA@BÜRO.example").unwrap(),
            "änna@büro.example"
        );
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        for raw in ["  A@B ", "Юзер@Почта.рф", "ΟΔΟΣ@x.gr", "İ@x.tr"] {
            let once = normalize_recovery_email(raw).unwrap();
            assert_eq!(normalize_recovery_email(&once).unwrap(), once);
        }
    }

    #[test]
    fn length_bounds_are_in_bytes_of_the_normalised_address() {
        assert_eq!(normalize_recovery_email("a@b").unwrap(), "a@b");
        assert_eq!(
            normalize_recovery_email("a@"),
            Err(RecoveryEmailError::TooShort)
        );
        assert_eq!(
            normalize_recovery_email("   "),
            Err(RecoveryEmailError::TooShort)
        );
        assert_eq!(
            normalize_recovery_email(""),
            Err(RecoveryEmailError::TooShort)
        );
        let longest = format!("{}@b.c", "a".repeat(RECOVERY_EMAIL_MAX_LEN - 4));
        assert_eq!(longest.len(), RECOVERY_EMAIL_MAX_LEN);
        assert_eq!(normalize_recovery_email(&longest).unwrap(), longest);
        let too_long = format!("a{longest}");
        assert_eq!(
            normalize_recovery_email(&too_long),
            Err(RecoveryEmailError::TooLong)
        );
        // Surrounding white space does not count against the bound.
        assert!(normalize_recovery_email(&format!("  {longest}  ")).is_ok());
    }

    #[test]
    fn the_at_sign_rule() {
        assert_eq!(
            normalize_recovery_email("alice.example.com"),
            Err(RecoveryEmailError::NotOneAtSign)
        );
        assert_eq!(
            normalize_recovery_email("a@b@c"),
            Err(RecoveryEmailError::NotOneAtSign)
        );
        assert_eq!(
            normalize_recovery_email("@example.com"),
            Err(RecoveryEmailError::EmptyPart)
        );
        assert_eq!(
            normalize_recovery_email("alice@"),
            Err(RecoveryEmailError::EmptyPart)
        );
    }

    #[test]
    fn control_characters_and_inner_white_space_are_refused() {
        assert_eq!(
            normalize_recovery_email("ali\u{0}ce@example.com"),
            Err(RecoveryEmailError::ControlCharacter)
        );
        assert_eq!(
            normalize_recovery_email("alice@exa\u{7f}mple.com"),
            Err(RecoveryEmailError::ControlCharacter)
        );
        assert_eq!(
            normalize_recovery_email("alice@exam\tple.com"),
            Err(RecoveryEmailError::ControlCharacter)
        );
        assert_eq!(
            normalize_recovery_email("ali ce@example.com"),
            Err(RecoveryEmailError::Whitespace)
        );
        assert_eq!(
            normalize_recovery_email("alice@exa\u{a0}mple.com"),
            Err(RecoveryEmailError::Whitespace)
        );
    }

    #[test]
    fn codes_are_stable() {
        let all = [
            (RecoveryEmailError::TooShort, "too-short"),
            (RecoveryEmailError::TooLong, "too-long"),
            (RecoveryEmailError::ControlCharacter, "control-character"),
            (RecoveryEmailError::Whitespace, "whitespace"),
            (RecoveryEmailError::NotOneAtSign, "not-one-at-sign"),
            (RecoveryEmailError::EmptyPart, "empty-part"),
        ];
        for (e, code) in all {
            assert_eq!(e.code(), code);
        }
    }
}
