// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the recovery-address rule (ADR-0075 section 4).
//! Mirror of `crates/lakko-ffi-uniffi/src/recovery_email.rs`.
//!
//! - `normalizeRecoveryEmail(address) -> string`: the one address rule, the
//!   function `services/auth` applies to build `email_key`.
//!
//! `emailVaultSeal` and `emailVaultOpen` were removed on 2026-11-26 with the
//! email vault (ADR-0075, amended the second time that day): a mailed code
//! never returns keys.
//!
//! Every refusal throws an `Error` whose message is `"<code>: <reason>"`.

use lakko_mnemonic::recovery_email::{self, RecoveryEmailError};
use wasm_bindgen::prelude::*;

fn address_msg(e: RecoveryEmailError) -> String {
    format!("{}: {e}", e.code())
}

fn normalize(address: &str) -> Result<String, String> {
    recovery_email::normalize_recovery_email(address).map_err(address_msg)
}

/// Normalise a recovery email address: trim, lower-case the whole address,
/// then require exactly one `@` with text on both sides, 3 to 254 bytes, no
/// control characters and no white space.
///
/// # Errors
///
/// Throws `"<code>: <reason>"` with the codes `too-short`, `too-long`,
/// `control-character`, `whitespace`, `not-one-at-sign`, `empty-part`.
#[wasm_bindgen(js_name = normalizeRecoveryEmail)]
pub fn normalize_recovery_email(address: &str) -> Result<String, JsError> {
    normalize(address).map_err(|m| JsError::new(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exported function builds a `JsError` on refusal, which only
    // exists on wasm32, so refusals are asserted on the inner function that
    // produces the message.

    fn code(msg: &str) -> &str {
        msg.split(": ").next().unwrap_or_default()
    }

    #[test]
    fn refusals_carry_a_code() {
        assert_eq!(
            normalize_recovery_email("  A@B.c ").unwrap(),
            "a@b.c".to_owned()
        );
        assert_eq!(code(&normalize("a b@c.d").unwrap_err()), "whitespace");
        assert_eq!(code(&normalize("@b").unwrap_err()), "too-short");
        assert_eq!(code(&normalize("@bc.d").unwrap_err()), "empty-part");
        assert_eq!(code(&normalize("a@b@c").unwrap_err()), "not-one-at-sign");
    }
}
