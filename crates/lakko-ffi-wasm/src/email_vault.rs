// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the email vault and the recovery-address rule
//! (ADR-0067 sections 2 and 4). Mirror of
//! `crates/lakko-ffi-uniffi/src/email_vault.rs`.
//!
//! - `emailVaultSeal(mnemonic, loginHandle, address, vaultSalt) ->
//!   Uint8Array`: the 44-byte blob `POST /recovery-email/enrol/finish`
//!   stores.
//! - `emailVaultOpen(loginHandle, address, vaultSalt, sealed) ->
//!   RecoveryMnemonic`: opens the blob `POST /recovery-email/recover/open`
//!   released.
//! - `normalizeRecoveryEmail(address) -> string`: the one address rule.
//!
//! Every refusal throws an `Error` whose message is `"<code>: <reason>"`.
//! Both vault calls run Argon2id (64 MiB) synchronously: call them from a
//! worker.

use lakko_mnemonic::recovery_email::{self, RecoveryEmailError};
use lakko_mnemonic::vault::{self, EmailVaultError, EMAIL_VAULT_SALT_LEN, LOGIN_HANDLE_LEN};
use lakko_mnemonic::Mnemonic;
use wasm_bindgen::prelude::*;

use crate::mnemonic::RecoveryMnemonic;

fn address_msg(e: RecoveryEmailError) -> String {
    format!("{}: {e}", e.code())
}

fn vault_msg(e: &EmailVaultError) -> String {
    format!("{}: {e}", e.code())
}

fn handle(login_handle: &[u8]) -> Result<[u8; LOGIN_HANDLE_LEN], String> {
    login_handle
        .try_into()
        .map_err(|_| "bad-handle-length: the login handle must be 32 bytes".to_owned())
}

fn salt(vault_salt: &[u8]) -> Result<[u8; EMAIL_VAULT_SALT_LEN], String> {
    vault_salt
        .try_into()
        .map_err(|_| "bad-salt-length: the vault salt must be 16 bytes".to_owned())
}

fn normalize(address: &str) -> Result<String, String> {
    recovery_email::normalize_recovery_email(address).map_err(address_msg)
}

fn seal(
    mnemonic: &Mnemonic,
    login_handle: &[u8],
    address: &str,
    vault_salt: &[u8],
) -> Result<Vec<u8>, String> {
    let handle = handle(login_handle)?;
    let salt = salt(vault_salt)?;
    vault::seal_entropy_under_email(mnemonic, &handle, address, &salt).map_err(|e| vault_msg(&e))
}

fn open(
    login_handle: &[u8],
    address: &str,
    vault_salt: &[u8],
    sealed: &[u8],
) -> Result<Mnemonic, String> {
    let handle = handle(login_handle)?;
    let salt = salt(vault_salt)?;
    vault::open_entropy_under_email(&handle, address, &salt, sealed).map_err(|e| vault_msg(&e))
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

/// Seal the mnemonic's entropy under the login handle (32 bytes), the
/// recovery email address and the server-issued vault salt (16 bytes). The
/// address is normalised here.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: an address code, `bad-handle-length`,
/// `bad-salt-length`, `kdf` or `seal`.
#[wasm_bindgen(js_name = emailVaultSeal)]
pub fn email_vault_seal(
    mnemonic: &RecoveryMnemonic,
    login_handle: &[u8],
    address: &str,
    vault_salt: &[u8],
) -> Result<Vec<u8>, JsError> {
    seal(mnemonic.inner(), login_handle, address, vault_salt).map_err(|m| JsError::new(&m))
}

/// Open an email-vault blob and rebuild the recovery phrase.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: `not-opened` when the blob does not open
/// under this handle, address and salt, whatever the reason; an address
/// code; `bad-handle-length`, `bad-salt-length` or `bad-sealed-length`.
#[wasm_bindgen(js_name = emailVaultOpen)]
pub fn email_vault_open(
    login_handle: &[u8],
    address: &str,
    vault_salt: &[u8],
    sealed: &[u8],
) -> Result<RecoveryMnemonic, JsError> {
    open(login_handle, address, vault_salt, sealed)
        .map(RecoveryMnemonic::from_inner)
        .map_err(|m| JsError::new(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exported functions build a `JsError` on refusal, which only
    // exists on wasm32, so refusals are asserted on the inner functions
    // that produce the message.

    fn code(msg: &str) -> &str {
        msg.split(": ").next().unwrap_or_default()
    }

    #[test]
    fn email_vault_round_trip_over_the_wasm_shape() {
        let m = RecoveryMnemonic::generate();
        let sealed = email_vault_seal(&m, &[0x11; 32], "Alice@Example.com", &[0x22; 16]).unwrap();
        assert_eq!(sealed.len(), 44);
        let back =
            email_vault_open(&[0x11; 32], " alice@example.COM ", &[0x22; 16], &sealed).unwrap();
        assert_eq!(back.words(), m.words());

        let wrong = open(&[0x11; 32], "bob@example.com", &[0x22; 16], &sealed).unwrap_err();
        assert_eq!(code(&wrong), "not-opened");
        let mut tampered = sealed;
        tampered[5] ^= 1;
        let damaged = open(&[0x11; 32], "alice@example.com", &[0x22; 16], &tampered).unwrap_err();
        // A damaged blob and a wrong address are the same message.
        assert_eq!(damaged, wrong);
    }

    #[test]
    fn refusals_carry_a_code() {
        let m = RecoveryMnemonic::generate();
        let a = "alice@example.com";
        assert_eq!(
            code(&seal(m.inner(), &[0; 31], a, &[0; 16]).unwrap_err()),
            "bad-handle-length"
        );
        assert_eq!(
            code(&seal(m.inner(), &[0; 32], a, &[0; 17]).unwrap_err()),
            "bad-salt-length"
        );
        assert_eq!(
            code(&seal(m.inner(), &[0; 32], "a@b@c", &[0; 16]).unwrap_err()),
            "not-one-at-sign"
        );
        assert_eq!(
            code(&open(&[0; 32], a, &[0; 16], &[0; 45]).unwrap_err()),
            "bad-sealed-length"
        );
        assert_eq!(
            normalize_recovery_email("  A@B.c ").unwrap(),
            "a@b.c".to_owned()
        );
        assert_eq!(code(&normalize("a b@c.d").unwrap_err()), "whitespace");
        assert_eq!(code(&normalize("@b").unwrap_err()), "too-short");
        assert_eq!(code(&normalize("@bc.d").unwrap_err()), "empty-part");
    }
}
