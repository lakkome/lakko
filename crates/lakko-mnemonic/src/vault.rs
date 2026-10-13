// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The two vaults that carry the recovery phrase's entropy: the passkey
//! vault (ADR-0072 decision 6) and the email vault (ADR-0075 section 2).
//!
//! ## Passkey vault
//!
//! [`seal_entropy_under_secret`] seals the 16-byte phrase entropy under a
//! key derived from a passkey's 32-byte WebAuthn PRF output;
//! [`open_entropy_under_secret`] opens it and rebuilds the phrase with
//! [`Mnemonic::from_entropy`], which refuses anything but 16 bytes. The
//! server stores the blob opaque and cannot search it: there is no
//! verifier, and the key comes from the authenticator.
//!
//! ## Email vault
//!
//! For an account whose owner has not saved a recovery phrase,
//! [`seal_entropy_under_email`] seals the same 16-byte entropy under a key
//! stretched from the account's login handle and its recovery email
//! address, and [`open_entropy_under_email`] opens it:
//!
//! ```text
//! vault_key = Argon2id(password = login_handle(32) ‖ len(2 BE) ‖ address,
//!                      salt     = "lakko/email-vault/v1" ‖ vault_salt(16))
//! seal_key  = HKDF-SHA-256(vault_key, salt = "", info = "lakko/email-vault/seal/v1")
//! sealed    = nonce(12) ‖ ChaCha20-Poly1305(seal_key, entropy(16))   (44 bytes)
//! ```
//!
//! `address` is the output of
//! [`normalize_recovery_email`](crate::recovery_email::normalize_recovery_email);
//! both functions normalise what they are given, so a caller cannot forget
//! to. The blob has the passkey vault's shape and length, and the distinct
//! HKDF `info` means neither kind of blob opens as the other even if the
//! same 32 bytes were ever used for both.
//!
//! **Argon2id parameters.** The cost is
//! [`Argon2Params::INTERACTIVE`](lakko_crypto::kdf::Argon2Params::INTERACTIVE)
//! (64 MiB, 3 passes, 4 lanes), the set the backup KEK uses. It was chosen
//! for the backup and is reused here; it has **not** been measured for this
//! use on the three client platforms (ADR-0075 sprint 1 leaves that
//! measurement open). The parameters are not carried in the blob, so
//! changing them is a format change.
//!
//! **What this protects against, and what it does not** (ADR-0075 section
//! 2): the input to Argon2id is a username-derived handle and an email
//! address, neither of which is a secret from the server operator, who sees
//! the address whenever it sends a letter. The stretch only slows someone
//! who holds a copy of the blobs without the addresses.
//!
//! A wrong address, a wrong handle, a wrong salt and a damaged blob are one
//! outcome, [`EmailVaultError::NotOpened`]; nothing tells them apart.
//!
//! The three-word check is not here: it runs on the device against the
//! phrase the device holds and involves no server and no sealed value, see
//! [`crate::word_check`].

use lakko_crypto::aead;
use lakko_crypto::kdf::{argon2id_kek_with_params, hkdf_sha256, Argon2Params};
use lakko_crypto::secret::SecretBytes;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::recovery_email::{normalize_recovery_email, RecoveryEmailError};
use crate::{Mnemonic, MnemonicError, ENTROPY_LEN};

const PASSKEY_SEAL_INFO: &[u8] = b"lakko/passkey-vault/seal/v1";

/// Prefix of the Argon2id salt of the email vault; the 16-byte
/// `vault_salt` follows it.
const EMAIL_VAULT_SALT_DOMAIN: &[u8] = b"lakko/email-vault/v1";

/// HKDF `info` separating the email vault's seal key from every other use
/// of the stretched key, and from the passkey vault's seal key.
const EMAIL_SEAL_INFO: &[u8] = b"lakko/email-vault/seal/v1";

/// The Argon2id cost of the email vault: the backup KEK's set. Chosen, not
/// measured for this use (see the module docs).
const EMAIL_VAULT_ARGON2: Argon2Params = Argon2Params::INTERACTIVE;

/// Length of a login handle (ADR-0054 S5), in bytes.
pub const LOGIN_HANDLE_LEN: usize = 32;

/// Length of the per-vault salt the server issues at enrolment, in bytes.
pub const EMAIL_VAULT_SALT_LEN: usize = 16;

/// Length of a sealed email vault: `nonce(12) ‖ entropy(16) ‖ tag(16)`. The
/// passkey vault's blob has the same length.
pub const EMAIL_VAULT_SEALED_LEN: usize = 12 + ENTROPY_LEN + 16;

/// Everything that goes wrong sealing or opening the passkey vault.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The HKDF step refused its parameters.
    #[error("key derivation failed")]
    Kdf,
    /// Sealing the entropy under the passkey secret failed.
    #[error("sealing the entropy failed")]
    Seal,
    /// The passkey blob did not open under this secret.
    #[error("the passkey did not open the stored keys")]
    Open,
    /// The opened bytes did not rebuild a mnemonic.
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
}

/// Seal the mnemonic's entropy under a 32-byte secret a passkey's WebAuthn
/// PRF extension produced (ADR-0049 section 3, ADR-0072 decision 6). The PRF output is stable
/// per credential and follows a synced passkey to a new device, so the
/// sealed blob, stored opaque server-side, lets that passkey carry the
/// identity. HKDF domain-separates the seal key from the PRF output.
///
/// # Errors
///
/// [`VaultError::Kdf`] / [`VaultError::Seal`] when the primitives refuse
/// (not reachable with a 32-byte input).
pub fn seal_entropy_under_secret(
    mnemonic: &Mnemonic,
    secret: &[u8; 32],
) -> Result<Vec<u8>, VaultError> {
    let seal_key: SecretBytes<32> =
        hkdf_sha256(secret, &[], PASSKEY_SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    let entropy = mnemonic.to_entropy();
    aead::seal_with_key(seal_key.as_bytes(), entropy.as_ref()).map_err(|_| VaultError::Seal)
}

/// Inverse of [`seal_entropy_under_secret`].
///
/// # Errors
///
/// [`VaultError::Open`] when the bytes do not authenticate under this
/// secret; [`VaultError::Mnemonic`] when the opened bytes are not 16-byte
/// entropy.
pub fn open_entropy_under_secret(secret: &[u8; 32], sealed: &[u8]) -> Result<Mnemonic, VaultError> {
    let seal_key: SecretBytes<32> =
        hkdf_sha256(secret, &[], PASSKEY_SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    let entropy = Zeroizing::new(
        aead::open_with_key(seal_key.as_bytes(), sealed).map_err(|_| VaultError::Open)?,
    );
    Ok(Mnemonic::from_entropy(&entropy)?)
}

/// Everything that goes wrong sealing or opening the email vault.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EmailVaultError {
    /// The address is not a recovery email address.
    #[error(transparent)]
    Address(#[from] RecoveryEmailError),
    /// The sealed blob is not [`EMAIL_VAULT_SEALED_LEN`] bytes long.
    #[error("the sealed vault must be 44 bytes, got {got}")]
    BadSealedLength {
        /// The length handed in.
        got: usize,
    },
    /// Argon2id or HKDF refused its parameters.
    #[error("key derivation failed")]
    Kdf,
    /// Sealing the entropy failed.
    #[error("sealing the entropy failed")]
    Seal,
    /// The vault did not open: a wrong address, a wrong login handle, a
    /// wrong salt or a damaged blob. Which of them is not reported.
    #[error("the email vault did not open")]
    NotOpened,
}

impl EmailVaultError {
    /// A stable kebab-case code for this refusal. An address refusal
    /// reports the [`RecoveryEmailError`] code. Frozen: clients branch on
    /// it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Address(e) => e.code(),
            Self::BadSealedLength { .. } => "bad-sealed-length",
            Self::Kdf => "kdf",
            Self::Seal => "seal",
            Self::NotOpened => "not-opened",
        }
    }
}

/// Stretch `(login_handle, address)` into the email vault's seal key. The
/// address is normalised here.
fn email_seal_key(
    login_handle: &[u8; LOGIN_HANDLE_LEN],
    address: &str,
    vault_salt: &[u8; EMAIL_VAULT_SALT_LEN],
) -> Result<SecretBytes<32>, EmailVaultError> {
    let address = normalize_recovery_email(address)?;
    // The normalised address is at most 254 bytes, so the length fits.
    let address_len = u16::try_from(address.len()).map_err(|_| RecoveryEmailError::TooLong)?;

    let mut password = Zeroizing::new(Vec::with_capacity(LOGIN_HANDLE_LEN + 2 + address.len()));
    password.extend_from_slice(login_handle);
    password.extend_from_slice(&address_len.to_be_bytes());
    password.extend_from_slice(address.as_bytes());

    let mut salt = [0u8; EMAIL_VAULT_SALT_DOMAIN.len() + EMAIL_VAULT_SALT_LEN];
    salt[..EMAIL_VAULT_SALT_DOMAIN.len()].copy_from_slice(EMAIL_VAULT_SALT_DOMAIN);
    salt[EMAIL_VAULT_SALT_DOMAIN.len()..].copy_from_slice(vault_salt);

    let vault_key = argon2id_kek_with_params(&password, &salt, EMAIL_VAULT_ARGON2)
        .map_err(|_| EmailVaultError::Kdf)?;
    seal_key_from_vault_key(vault_key.as_bytes())
}

/// The HKDF step between the stretched key and the AEAD key.
fn seal_key_from_vault_key(vault_key: &[u8; 32]) -> Result<SecretBytes<32>, EmailVaultError> {
    hkdf_sha256(vault_key, &[], EMAIL_SEAL_INFO).map_err(|_| EmailVaultError::Kdf)
}

/// Open a sealed blob under an already derived seal key. Any failure past
/// the length gate is [`EmailVaultError::NotOpened`].
fn open_under_seal_key(seal_key: &[u8; 32], sealed: &[u8]) -> Result<Mnemonic, EmailVaultError> {
    if sealed.len() != EMAIL_VAULT_SEALED_LEN {
        return Err(EmailVaultError::BadSealedLength { got: sealed.len() });
    }
    let entropy = Zeroizing::new(
        aead::open_with_key(seal_key, sealed).map_err(|_| EmailVaultError::NotOpened)?,
    );
    Mnemonic::from_entropy(&entropy).map_err(|_| EmailVaultError::NotOpened)
}

/// Seal the mnemonic's entropy under the account's login handle and
/// recovery email address (ADR-0075 section 2). `vault_salt` is the 16
/// bytes `POST /recovery-email/enrol/start` answered. The result is the
/// 44-byte blob the server stores for a mail-protected account.
///
/// The address is normalised with
/// [`normalize_recovery_email`](crate::recovery_email::normalize_recovery_email)
/// before use. This runs Argon2id at the backup's cost (64 MiB).
///
/// # Errors
///
/// [`EmailVaultError::Address`] when the address is refused;
/// [`EmailVaultError::Kdf`] / [`EmailVaultError::Seal`] when the primitives
/// refuse (not reachable with these inputs).
pub fn seal_entropy_under_email(
    mnemonic: &Mnemonic,
    login_handle: &[u8; LOGIN_HANDLE_LEN],
    address: &str,
    vault_salt: &[u8; EMAIL_VAULT_SALT_LEN],
) -> Result<Vec<u8>, EmailVaultError> {
    let seal_key = email_seal_key(login_handle, address, vault_salt)?;
    let entropy = mnemonic.to_entropy();
    aead::seal_with_key(seal_key.as_bytes(), entropy.as_ref()).map_err(|_| EmailVaultError::Seal)
}

/// Inverse of [`seal_entropy_under_email`]: open the blob the server
/// released after a verified mailed code and rebuild the recovery phrase.
/// The caller must still check the phrase against the account's published
/// identity before using it.
///
/// The blob's length is checked before Argon2id runs.
///
/// # Errors
///
/// [`EmailVaultError::Address`] when the address is refused;
/// [`EmailVaultError::BadSealedLength`] when `sealed` is not 44 bytes;
/// [`EmailVaultError::NotOpened`] when the blob does not open under this
/// handle, address and salt, whatever the reason.
pub fn open_entropy_under_email(
    login_handle: &[u8; LOGIN_HANDLE_LEN],
    address: &str,
    vault_salt: &[u8; EMAIL_VAULT_SALT_LEN],
    sealed: &[u8],
) -> Result<Mnemonic, EmailVaultError> {
    // Refuse the address and the length before paying for Argon2id.
    normalize_recovery_email(address)?;
    if sealed.len() != EMAIL_VAULT_SEALED_LEN {
        return Err(EmailVaultError::BadSealedLength { got: sealed.len() });
    }
    let seal_key = email_seal_key(login_handle, address, vault_salt)?;
    open_under_seal_key(seal_key.as_bytes(), sealed)
}

/// Fuzz-only: open `sealed` as an email vault under a fixed stretched key,
/// skipping Argon2id (its input is not the attacker's). Returns whether it
/// opened.
pub(crate) fn fuzz_open_email_vault(sealed: &[u8]) -> bool {
    seal_key_from_vault_key(&[0x42u8; 32])
        .and_then(|key| open_under_seal_key(key.as_bytes(), sealed))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkey_seal_round_trips_and_refuses_another_secret() {
        let m = Mnemonic::generate();
        let secret = [0x77u8; 32];
        let sealed = seal_entropy_under_secret(&m, &secret).unwrap();
        // nonce(12) + entropy(16) + tag(16)
        assert_eq!(sealed.len(), 44);
        assert_eq!(
            open_entropy_under_secret(&secret, &sealed)
                .unwrap()
                .phrase(),
            m.phrase()
        );
        assert!(matches!(
            open_entropy_under_secret(&[0x78u8; 32], &sealed),
            Err(VaultError::Open)
        ));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(matches!(
            open_entropy_under_secret(&secret, &tampered),
            Err(VaultError::Open)
        ));
        // A PRF output is never used as the seal key directly.
        assert!(aead::open_with_key(&secret, &sealed).is_err());
    }

    #[test]
    fn a_passkey_blob_holding_32_bytes_fails_closed() {
        // What a blob sealed for a 24-word phrase looked like: it
        // authenticates under the right secret and is still refused.
        let secret = [0x21u8; 32];
        let seal_key: SecretBytes<32> = hkdf_sha256(&secret, &[], PASSKEY_SEAL_INFO).unwrap();
        let old = aead::seal_with_key(seal_key.as_bytes(), &[0xABu8; 32]).unwrap();
        assert!(matches!(
            open_entropy_under_secret(&secret, &old),
            Err(VaultError::Mnemonic(_))
        ));
    }

    // ---- email vault (ADR-0075) -------------------------------------

    const HANDLE: [u8; LOGIN_HANDLE_LEN] = [0x11; LOGIN_HANDLE_LEN];
    const SALT: [u8; EMAIL_VAULT_SALT_LEN] = [0x22; EMAIL_VAULT_SALT_LEN];
    const ADDRESS: &str = "alice@example.com";
    const ENTROPY: [u8; ENTROPY_LEN] = [0x24; ENTROPY_LEN];

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// One Argon2id run shared by the tests that only need a valid blob.
    fn sealed_fixture() -> &'static (Mnemonic, Vec<u8>) {
        static FIXTURE: std::sync::OnceLock<(Mnemonic, Vec<u8>)> = std::sync::OnceLock::new();
        FIXTURE.get_or_init(|| {
            let m = Mnemonic::from_entropy(&ENTROPY).unwrap();
            let sealed = seal_entropy_under_email(&m, &HANDLE, ADDRESS, &SALT).unwrap();
            (m, sealed)
        })
    }

    #[test]
    fn email_vault_round_trips_and_normalises_the_address() {
        let (m, sealed) = sealed_fixture();
        assert_eq!(sealed.len(), EMAIL_VAULT_SEALED_LEN);
        assert_eq!(EMAIL_VAULT_SEALED_LEN, 44);
        // Opened with a differently spelled but equal address.
        let back =
            open_entropy_under_email(&HANDLE, "  Alice@Example.COM ", &SALT, sealed).unwrap();
        assert_eq!(back.phrase(), m.phrase());
    }

    #[test]
    fn email_vault_does_not_open_under_another_address() {
        let (_, sealed) = sealed_fixture();
        assert!(matches!(
            open_entropy_under_email(&HANDLE, "alicf@example.com", &SALT, sealed),
            Err(EmailVaultError::NotOpened)
        ));
    }

    #[test]
    fn email_vault_does_not_open_under_another_handle() {
        let (_, sealed) = sealed_fixture();
        assert!(matches!(
            open_entropy_under_email(&[0x12; 32], ADDRESS, &SALT, sealed),
            Err(EmailVaultError::NotOpened)
        ));
    }

    #[test]
    fn email_vault_does_not_open_under_another_salt() {
        let (_, sealed) = sealed_fixture();
        assert!(matches!(
            open_entropy_under_email(&HANDLE, ADDRESS, &[0x23; 16], sealed),
            Err(EmailVaultError::NotOpened)
        ));
    }

    #[test]
    fn a_damaged_email_vault_is_the_same_refusal_as_a_wrong_address() {
        let (_, sealed) = sealed_fixture();
        let mut tampered = sealed.clone();
        tampered[20] ^= 1;
        let e = open_entropy_under_email(&HANDLE, ADDRESS, &SALT, &tampered).unwrap_err();
        assert!(matches!(e, EmailVaultError::NotOpened));
        assert_eq!(e.code(), "not-opened");
    }

    #[test]
    fn malformed_email_vault_input_is_typed_and_costs_no_argon2() {
        let m = Mnemonic::from_entropy(&ENTROPY).unwrap();
        for len in [0usize, 1, 28, 43, 45, 60] {
            let e = open_entropy_under_email(&HANDLE, ADDRESS, &SALT, &vec![0u8; len]).unwrap_err();
            assert!(matches!(e, EmailVaultError::BadSealedLength { got } if got == len));
            assert_eq!(e.code(), "bad-sealed-length");
        }
        let e = open_entropy_under_email(&HANDLE, "no-at-sign", &SALT, &[0u8; 44]).unwrap_err();
        assert!(matches!(
            e,
            EmailVaultError::Address(RecoveryEmailError::NotOneAtSign)
        ));
        assert_eq!(e.code(), "not-one-at-sign");
        assert!(matches!(
            seal_entropy_under_email(&m, &HANDLE, "a b@c", &SALT),
            Err(EmailVaultError::Address(RecoveryEmailError::Whitespace))
        ));
    }

    #[test]
    fn passkey_and_email_blobs_do_not_open_each_other() {
        let m = Mnemonic::from_entropy(&ENTROPY).unwrap();
        // The same 32 bytes as the passkey secret and as the stretched
        // email-vault key: only the HKDF info differs.
        let shared = [0x5au8; 32];
        let passkey_blob = seal_entropy_under_secret(&m, &shared).unwrap();
        let email_key = seal_key_from_vault_key(&shared).unwrap();
        let email_blob = aead::seal_with_key(email_key.as_bytes(), &ENTROPY).unwrap();
        assert_eq!(passkey_blob.len(), email_blob.len());

        assert!(open_under_seal_key(email_key.as_bytes(), &email_blob).is_ok());
        assert!(matches!(
            open_under_seal_key(email_key.as_bytes(), &passkey_blob),
            Err(EmailVaultError::NotOpened)
        ));
        assert!(matches!(
            open_entropy_under_secret(&shared, &email_blob),
            Err(VaultError::Open)
        ));

        // And through the public pair: a real email blob under a passkey
        // secret, a passkey blob under the email inputs.
        let (_, sealed) = sealed_fixture();
        assert!(matches!(
            open_entropy_under_secret(&shared, sealed),
            Err(VaultError::Open)
        ));
        assert!(matches!(
            open_entropy_under_email(&HANDLE, ADDRESS, &SALT, &passkey_blob),
            Err(EmailVaultError::NotOpened)
        ));
    }

    #[test]
    fn an_email_vault_holding_other_than_16_bytes_cannot_be_presented() {
        // 44 bytes is the only accepted length, which fixes the plaintext
        // at 16 bytes; anything else is refused before the key is used.
        let key = seal_key_from_vault_key(&[0x01; 32]).unwrap();
        let long = aead::seal_with_key(key.as_bytes(), &[0xAB; 32]).unwrap();
        assert!(matches!(
            open_under_seal_key(key.as_bytes(), &long),
            Err(EmailVaultError::BadSealedLength { got: 60 })
        ));
    }

    #[test]
    fn golden_pins_the_email_vault_derivation() {
        // handle = 0x11 * 32, address = "alice@example.com", salt = 0x22 * 16.
        let key = email_seal_key(&HANDLE, " Alice@Example.com", &SALT).unwrap();
        assert_eq!(hex(key.as_bytes()), GOLDEN_SEAL_KEY_HEX);
        // A blob sealed once under that key (entropy = 0x24 * 16).
        let sealed = unhex(GOLDEN_SEALED_HEX);
        let m = open_under_seal_key(key.as_bytes(), &sealed).unwrap();
        assert_eq!(m.to_entropy().as_ref(), &ENTROPY);
    }

    const GOLDEN_SEAL_KEY_HEX: &str =
        "32281affd7959274b5fead17a41c056b96eaa8640ace49f97bd289e0b7ec4ab6";
    const GOLDEN_SEALED_HEX: &str =
        "328282d117ff6872896e1dc1025297dc4c0e52fb4cd642099e12e5e4c64c1a57895b365dc686b6195db90f16";
}
