// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the email vault and the recovery-address rule
//! (ADR-0075 sections 2 and 4). Mirror of
//! `crates/lakko-ffi-wasm/src/email_vault.rs`.
//!
//! [`email_vault_seal`] seals the phrase entropy under the account's login
//! handle and recovery email address into the 44-byte blob
//! `POST /recovery-email/enrol/finish` stores; [`email_vault_open`] opens
//! the blob `POST /recovery-email/recover/open` released.
//! [`normalize_recovery_email`] is the one address rule, the same function
//! `services/auth` applies. Both vault calls run Argon2id (64 MiB): call
//! them off the main thread.

use std::sync::Arc;

use lakko_mnemonic::recovery_email::{self, RecoveryEmailError};
use lakko_mnemonic::vault::{self, EmailVaultError, EMAIL_VAULT_SALT_LEN, LOGIN_HANDLE_LEN};

use crate::mnemonic::RecoveryMnemonic;
use crate::FfiError;

fn address_err(e: RecoveryEmailError) -> FfiError {
    FfiError::RecoveryEmail(format!("{}: {e}", e.code()))
}

fn vault_err(e: &EmailVaultError) -> FfiError {
    match e {
        EmailVaultError::NotOpened => FfiError::EmailVaultNotOpened,
        EmailVaultError::Address(a) => address_err(*a),
        other => FfiError::EmailVault(format!("{}: {other}", other.code())),
    }
}

fn handle(login_handle: &[u8]) -> Result<[u8; LOGIN_HANDLE_LEN], FfiError> {
    login_handle.try_into().map_err(|_| {
        FfiError::EmailVault("bad-handle-length: the login handle must be 32 bytes".into())
    })
}

fn salt(vault_salt: &[u8]) -> Result<[u8; EMAIL_VAULT_SALT_LEN], FfiError> {
    vault_salt.try_into().map_err(|_| {
        FfiError::EmailVault("bad-salt-length: the vault salt must be 16 bytes".into())
    })
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

/// Seal the mnemonic's entropy under the login handle (32 bytes), the
/// recovery email address and the server-issued vault salt (16 bytes). The
/// result is the 44-byte blob the server stores. The address is normalised
/// here.
///
/// # Errors
///
/// [`FfiError::RecoveryEmail`] when the address is refused;
/// [`FfiError::EmailVault`] when the handle or the salt has the wrong
/// length (`bad-handle-length`, `bad-salt-length`) or the primitives refuse
/// (`kdf`, `seal`).
#[uniffi::export]
pub fn email_vault_seal(
    mnemonic: Arc<RecoveryMnemonic>,
    login_handle: Vec<u8>,
    address: String,
    vault_salt: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let handle = handle(&login_handle)?;
    let salt = salt(&vault_salt)?;
    vault::seal_entropy_under_email(mnemonic.inner(), &handle, &address, &salt)
        .map_err(|e| vault_err(&e))
}

/// Open an email-vault blob and rebuild the recovery phrase. The caller
/// must still check the phrase against the account's published identity
/// before using it.
///
/// # Errors
///
/// [`FfiError::EmailVaultNotOpened`] when the blob does not open under this
/// handle, address and salt, whatever the reason (a wrong address, a wrong
/// username, a damaged blob); [`FfiError::RecoveryEmail`] when the address
/// is refused; [`FfiError::EmailVault`] when an input has the wrong length
/// (`bad-handle-length`, `bad-salt-length`, `bad-sealed-length`).
#[uniffi::export]
pub fn email_vault_open(
    login_handle: Vec<u8>,
    address: String,
    vault_salt: Vec<u8>,
    sealed: Vec<u8>,
) -> Result<Arc<RecoveryMnemonic>, FfiError> {
    let handle = handle(&login_handle)?;
    let salt = salt(&vault_salt)?;
    let inner = vault::open_entropy_under_email(&handle, &address, &salt, &sealed)
        .map_err(|e| vault_err(&e))?;
    Ok(Arc::new(RecoveryMnemonic::from_inner(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Arc<RecoveryMnemonic>, Vec<u8>) {
        let m = Arc::new(RecoveryMnemonic::generate());
        let sealed = email_vault_seal(
            m.clone(),
            vec![0x11; 32],
            "Alice@Example.com".into(),
            vec![0x22; 16],
        )
        .unwrap();
        (m, sealed)
    }

    #[test]
    fn email_vault_round_trips_and_refuses_as_one_outcome() {
        let (m, sealed) = fixture();
        assert_eq!(sealed.len(), 44);
        let back = email_vault_open(
            vec![0x11; 32],
            " alice@example.COM".into(),
            vec![0x22; 16],
            sealed.clone(),
        )
        .unwrap();
        assert_eq!(back.phrase(), m.phrase());

        // Wrong address, wrong handle and a damaged blob: the same variant.
        assert!(matches!(
            email_vault_open(
                vec![0x11; 32],
                "bob@example.com".into(),
                vec![0x22; 16],
                sealed.clone()
            ),
            Err(FfiError::EmailVaultNotOpened)
        ));
        assert!(matches!(
            email_vault_open(
                vec![0x12; 32],
                "alice@example.com".into(),
                vec![0x22; 16],
                sealed.clone()
            ),
            Err(FfiError::EmailVaultNotOpened)
        ));
        let mut tampered = sealed;
        tampered[30] ^= 1;
        assert!(matches!(
            email_vault_open(
                vec![0x11; 32],
                "alice@example.com".into(),
                vec![0x22; 16],
                tampered
            ),
            Err(FfiError::EmailVaultNotOpened)
        ));
    }

    fn code_of<T>(r: Result<T, FfiError>) -> String {
        match r {
            Err(FfiError::EmailVault(msg) | FfiError::RecoveryEmail(msg)) => {
                msg.split(": ").next().unwrap_or_default().to_owned()
            },
            Err(other) => panic!("unexpected error {other:?}"),
            Ok(_) => panic!("expected a refusal"),
        }
    }

    #[test]
    fn malformed_inputs_are_typed() {
        let m = Arc::new(RecoveryMnemonic::generate());
        let addr = || "alice@example.com".to_owned();
        assert_eq!(
            code_of(email_vault_seal(
                m.clone(),
                vec![0; 31],
                addr(),
                vec![0; 16]
            )),
            "bad-handle-length"
        );
        assert_eq!(
            code_of(email_vault_seal(
                m.clone(),
                vec![0; 32],
                addr(),
                vec![0; 15]
            )),
            "bad-salt-length"
        );
        assert_eq!(
            code_of(email_vault_open(
                vec![0; 32],
                addr(),
                vec![0; 16],
                vec![0; 43]
            )),
            "bad-sealed-length"
        );
        assert!(matches!(
            email_vault_seal(m, vec![0; 32], "nope".into(), vec![0; 16]),
            Err(FfiError::RecoveryEmail(msg)) if msg.starts_with("not-one-at-sign: ")
        ));
        assert!(matches!(
            email_vault_open(vec![0; 32], "a@".into(), vec![0; 16], vec![0; 44]),
            Err(FfiError::RecoveryEmail(msg)) if msg.starts_with("too-short: ")
        ));
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
