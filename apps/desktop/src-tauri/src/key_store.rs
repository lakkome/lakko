// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where the device root key rests between launches (ADR-0085 section 2,
//! decisions 9.1 and 9.2).
//!
//! macOS: the 32-byte key is encrypted to a Secure Enclave P-256 key (ECIES,
//! `kSecKeyAlgorithmECIESEncryptionCofactorVariableIVX963SHA256AESGCM`) and
//! the result kept as a generic-password item, one per account. A permanent
//! Secure Enclave key needs the app signed with a keychain entitlement;
//! measured 2026-10-16, an unsigned binary gets `-34018`
//! (errSecMissingEntitlement). So:
//! - a release build that cannot create or find the Secure Enclave key
//!   refuses (`hardware-key-unavailable`): a signed build whose entitlement is
//!   broken must not drop to software protection without anyone noticing;
//! - a debug build falls back to keeping the key itself in the login
//!   keychain, logged, so the spike runs unsigned.
//!
//! Windows and Linux: the key sits in the operating system's software store
//! (Credential Manager; the Secret Service), the case decision 9.2 records as
//! an exception to invariant #7. The TPM path on Windows is not built. A Linux
//! with no keyring service refuses; nothing falls back to a file.
//!
//! Item value layout: `tag(1) || payload`; tag `0x01` = Secure Enclave
//! wrapped, `0x00` = the raw key (debug fallback only).

use std::fmt;

use zeroize::Zeroizing;

/// Why the store could not do what was asked.
#[derive(Debug)]
pub enum KeyStoreError {
    /// No store this host can use; the page shows the refusal screen.
    Unavailable(&'static str),
    /// The store exists but the operation failed.
    Failed(String),
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(r) => write!(f, "key store unavailable: {r}"),
            Self::Failed(e) => write!(f, "key store failed: {e}"),
        }
    }
}

type Key = Zeroizing<[u8; 32]>;

#[cfg(target_os = "macos")]
pub use macos::{delete, load, save};

#[cfg(not(target_os = "macos"))]
pub use other::{delete, load, save};

#[cfg(not(target_os = "macos"))]
mod other {
    use keyring::{Entry, Error};
    use zeroize::Zeroizing;

    use super::{Key, KeyStoreError};

    const SERVICE: &str = "me.lakko.desktop.device-key";

    /// No store at all is the refusal screen, never a file (`desktop.md`
    /// section 1.1); anything else is a failed operation.
    fn store_error(e: Error) -> KeyStoreError {
        match e {
            Error::NoStorageAccess(_) | Error::PlatformFailure(_) => {
                KeyStoreError::Unavailable("no-key-store")
            },
            e => KeyStoreError::Failed(e.to_string()),
        }
    }

    fn entry(account: &str) -> Result<Entry, KeyStoreError> {
        Entry::new(SERVICE, account).map_err(store_error)
    }

    pub fn save(account: &str, key: &Key) -> Result<(), KeyStoreError> {
        entry(account)?
            .set_secret(key.as_slice())
            .map_err(store_error)
    }

    pub fn load(account: &str) -> Result<Option<Key>, KeyStoreError> {
        match entry(account)?.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let key: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| KeyStoreError::Failed("stored key has the wrong length".into()))?;
                Ok(Some(Zeroizing::new(key)))
            },
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    pub fn delete(account: &str) -> Result<(), KeyStoreError> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(store_error(e)),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use security_framework::item::{
        ItemClass, ItemSearchOptions, Location, Reference, SearchResult,
    };
    use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };
    use tracing::warn;
    use zeroize::Zeroizing;

    use super::{Key, KeyStoreError};

    const SERVICE: &str = "me.lakko.desktop.device-key";
    const WRAP_LABEL: &str = "me.lakko.desktop.device-wrap";
    const TAG_RAW: u8 = 0x00;
    const TAG_SE: u8 = 0x01;
    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;
    /// `errSecMissingEntitlement`.
    const MISSING_ENTITLEMENT: i32 = -34018;

    const ALG: Algorithm = Algorithm::ECIESEncryptionCofactorVariableIVX963SHA256AESGCM;

    /// The Secure Enclave wrapping key, found or created; `Ok(None)` when the
    /// process may not keep one (unsigned).
    fn wrap_key() -> Result<Option<SecKey>, KeyStoreError> {
        let found = ItemSearchOptions::new()
            .class(ItemClass::key())
            .label(WRAP_LABEL)
            .load_refs(true)
            .search();
        if let Ok(results) = found {
            for r in results {
                if let SearchResult::Ref(Reference::Key(k)) = r {
                    return Ok(Some(k));
                }
            }
        }
        let mut opts = GenerateKeyOptions::default();
        opts.set_key_type(KeyType::ec_sec_prime_random())
            .set_size_in_bits(256)
            .set_token(Token::SecureEnclave)
            .set_label(WRAP_LABEL)
            .set_location(Location::DataProtectionKeychain);
        match SecKey::new(&opts) {
            Ok(k) => Ok(Some(k)),
            Err(e) if e.code() == MISSING_ENTITLEMENT as isize => Ok(None),
            Err(e) => Err(KeyStoreError::Failed(format!(
                "secure enclave key: {}",
                e.code()
            ))),
        }
    }

    fn software_fallback_allowed() -> Result<(), KeyStoreError> {
        if cfg!(debug_assertions) {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                warn!("no Secure Enclave key for an unsigned build; keeping the device key in the login keychain (debug only)");
            });
            Ok(())
        } else {
            Err(KeyStoreError::Unavailable("hardware-key-unavailable"))
        }
    }

    pub fn save(account: &str, key: &Key) -> Result<(), KeyStoreError> {
        let value = match wrap_key()? {
            Some(wrap) => {
                let public = wrap
                    .public_key()
                    .ok_or_else(|| KeyStoreError::Failed("no public key".into()))?;
                let wrapped = public
                    .encrypt_data(ALG, key.as_ref())
                    .map_err(|e| KeyStoreError::Failed(format!("wrap: {}", e.code())))?;
                let mut v = Zeroizing::new(Vec::with_capacity(1 + wrapped.len()));
                v.push(TAG_SE);
                v.extend_from_slice(&wrapped);
                v
            },
            None => {
                software_fallback_allowed()?;
                let mut v = Zeroizing::new(Vec::with_capacity(33));
                v.push(TAG_RAW);
                v.extend_from_slice(key.as_ref());
                v
            },
        };
        set_generic_password(SERVICE, account, &value)
            .map_err(|e| KeyStoreError::Failed(format!("keychain write: {}", e.code())))
    }

    pub fn load(account: &str) -> Result<Option<Key>, KeyStoreError> {
        let value = match get_generic_password(SERVICE, account) {
            Ok(v) => Zeroizing::new(v),
            Err(e) if e.code() == NOT_FOUND => return Ok(None),
            Err(e) => {
                return Err(KeyStoreError::Failed(format!(
                    "keychain read: {}",
                    e.code()
                )))
            },
        };
        let (tag, payload) = value
            .split_first()
            .ok_or_else(|| KeyStoreError::Failed("empty item".into()))?;
        let raw: Zeroizing<Vec<u8>> = match *tag {
            TAG_SE => {
                let wrap =
                    wrap_key()?.ok_or(KeyStoreError::Unavailable("hardware-key-unavailable"))?;
                Zeroizing::new(
                    wrap.decrypt_data(ALG, payload)
                        .map_err(|e| KeyStoreError::Failed(format!("unwrap: {}", e.code())))?,
                )
            },
            TAG_RAW => {
                software_fallback_allowed()?;
                Zeroizing::new(payload.to_vec())
            },
            _ => return Err(KeyStoreError::Failed("unknown item tag".into())),
        };
        let mut key = Zeroizing::new([0u8; 32]);
        if raw.len() != key.len() {
            return Err(KeyStoreError::Failed("bad key length".into()));
        }
        key.copy_from_slice(&raw);
        Ok(Some(key))
    }

    pub fn delete(account: &str) -> Result<(), KeyStoreError> {
        match delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(KeyStoreError::Failed(format!(
                "keychain delete: {}",
                e.code()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real keychain of this machine, so it runs only on request:
    /// `cargo test -- --ignored key_store`, outside any sandbox.
    #[test]
    #[ignore = "uses the machine's keychain"]
    fn save_load_delete_round_trip() {
        let account = "lakko-desktop-key-store-selftest";
        let key = Zeroizing::new([42u8; 32]);
        let _ = delete(account);
        save(account, &key).expect("save");
        let loaded = load(account).expect("load").expect("present");
        assert_eq!(*loaded, *key);
        delete(account).expect("delete");
        assert!(load(account).expect("load after delete").is_none());
    }
}
