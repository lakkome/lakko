// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the recovery-address rule (ADR-0075 section 4).
//! Mirror of `crates/lakko-ffi-wasm/src/recovery_email.rs`.
//!
//! [`normalize_recovery_email`] is the one address rule, the same function
//! `services/auth` applies. `emailVaultSeal` and `emailVaultOpen` were
//! removed on 2026-11-26 with the email vault (ADR-0075, amended the second
//! time that day); their two `FfiError` variants stay, never produced, for
//! the ordinal reason.

use lakko_mnemonic::recovery_email::{self, RecoveryEmailError};

use crate::FfiError;

fn address_err(e: RecoveryEmailError) -> FfiError {
    FfiError::RecoveryEmail(format!("{}: {e}", e.code()))
}

/// Normalise a recovery email address: trim, lower-case the whole address,
/// then require exactly one `@` with text on both sides, 3 to 254 bytes, no
/// control characters and no white space.
///
/// # Errors
///
/// [`FfiError::RecoveryEmail`] with a message `"<code>: <reason>"`; the
/// codes are `too-short`, `too-long`, `control-character`, `whitespace`,
/// `not-one-at-sign` and `empty-part`.
#[uniffi::export]
pub fn normalize_recovery_email(address: String) -> Result<String, FfiError> {
    recovery_email::normalize_recovery_email(&address).map_err(address_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of<T>(r: Result<T, FfiError>) -> String {
        match r {
            Err(FfiError::RecoveryEmail(msg)) => {
                msg.split(": ").next().unwrap_or_default().to_owned()
            },
            Err(other) => panic!("unexpected error {other:?}"),
            Ok(_) => panic!("expected a refusal"),
        }
    }

    #[test]
    fn normalize_recovery_email_over_the_ffi() {
        assert_eq!(
            normalize_recovery_email("  Alice@Example.COM ".into()).unwrap(),
            "alice@example.com"
        );
        for (raw, code) in [
            ("a@", "too-short"),
            ("a@b@c", "not-one-at-sign"),
            ("@example.com", "empty-part"),
            ("a b@c.d", "whitespace"),
            ("a\u{1}b@c.d", "control-character"),
        ] {
            assert_eq!(code_of(normalize_recovery_email(raw.into())), code);
        }
        let long = format!("{}@b.c", "a".repeat(251));
        assert_eq!(code_of(normalize_recovery_email(long)), "too-long");
    }
}
