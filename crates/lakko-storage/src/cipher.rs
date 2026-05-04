// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Application-layer AEAD wrapper around any [`Store`].
//!
//! ## Why app-level instead of SQLCipher
//!
//! SQLCipher (Zetetic) encrypts whole DB pages at the C library
//! level. It's the textbook answer but pulls in OpenSSL and a
//! cross-compile headache for our iOS / Android / Tauri targets.
//! At the threat-model level we don't actually need full-page
//! encryption — the Lakko local DB only ever stores values that are
//! already opaque to outsiders (encrypted MLS state, content
//! addressed by pseudonym), and the row keys are themselves
//! pseudonyms (`group_id`, `device_id`, `oprf_hash`) with no
//! plaintext semantics. App-layer AEAD on `value` gives us the
//! same effective property at a fraction of the build complexity.
//!
//! Phase 2 may add SQLCipher on top if a concrete attacker model
//! demands it (e.g. forensic recovery from a wiped device).
//!
//! ## On-wire format
//!
//! Each stored cell is `nonce(12) ‖ ciphertext_with_tag`. The nonce
//! is freshly random per-write — collision probability across the
//! lifetime of any one device is negligible at 96 bits.
//!
//! ## Key hierarchy
//!
//! Caller passes a single 32-byte `root_key`. We derive a per-table
//! subkey via HKDF-SHA-256 with `info = "lakko-storage/v1/<table>"`.
//! This means a leaked Messages row cannot decrypt a Settings row
//! with the same logical key, and vice versa.
//!
//! AAD on every cell binds the ciphertext to its location:
//! `aad = "lakko-storage/v1/" ‖ table_name ‖ 0x00 ‖ key`. Moving a
//! row to a different `(table, key)` slot — by an attacker with
//! filesystem access but not the root key — fails to decrypt.

use std::sync::{Arc, Mutex};

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, KEY_LEN, NONCE_LEN};
use lakko_crypto::kdf;
use lakko_crypto::random;

use crate::error::{Result, StorageError};
use crate::store::{Store, Table};

const ALG: Algorithm = Algorithm::ChaCha20Poly1305;
const HKDF_INFO_PREFIX: &[u8] = b"lakko-storage/v1/subkey/";
const AAD_PREFIX: &[u8] = b"lakko-storage/v1/";

/// Wraps any `Store` and AEAD-encrypts every value with per-table
/// subkeys derived from `root_key`.
///
/// Cloning is cheap — the inner store is held behind `Arc` and the
/// per-table subkey cache is a small `Mutex<HashMap>` shared between
/// clones.
pub struct EncryptedStore {
    inner: Arc<dyn Store>,
    /// Per-table subkey cache. The HKDF derivation is cheap (~µs)
    /// but we still cache it to avoid recomputing on every call.
    subkeys: Arc<Mutex<std::collections::HashMap<Table, AeadKey>>>,
    /// Root key, kept for lazily deriving new subkeys. Wrapped in
    /// the same `Arc<Mutex>` borrow as the cache to avoid juggling
    /// two locks.
    root_key: AeadKey,
}

impl EncryptedStore {
    /// Wrap `inner` with AEAD. `root_key` should ultimately come from
    /// the platform's hardware-backed secure element (Secure Enclave,
    /// StrongBox, non-extractable WebCrypto, …) — see
    /// [crate-level docs](crate) for the hardware-key plan in 1.13+.
    /// For now the caller is responsible for sourcing it.
    pub fn new(inner: Arc<dyn Store>, root_key: [u8; KEY_LEN]) -> Self {
        Self {
            inner,
            subkeys: Arc::new(Mutex::new(std::collections::HashMap::new())),
            root_key: AeadKey::new(root_key),
        }
    }

    /// Derive (and cache) the per-table subkey.
    fn subkey_for(&self, table: Table) -> Result<AeadKey> {
        let mut guard = self.subkeys.lock().expect("subkey cache poisoned");
        if let Some(k) = guard.get(&table) {
            return Ok(k.clone());
        }
        let mut info = Vec::with_capacity(HKDF_INFO_PREFIX.len() + table.name().len());
        info.extend_from_slice(HKDF_INFO_PREFIX);
        info.extend_from_slice(table.name().as_bytes());
        let derived: lakko_crypto::secret::SecretBytes<KEY_LEN> =
            kdf::hkdf_sha256(self.root_key.as_bytes(), &[], &info)
                .map_err(|e| StorageError::Cipher(format!("hkdf for table subkey: {e}")))?;
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(derived.as_bytes());
        let aead_key = AeadKey::new(key);
        guard.insert(table, aead_key.clone());
        Ok(aead_key)
    }

    /// Build the AAD for a cell. `aad = AAD_PREFIX ‖ table_name ‖ 0x00 ‖ key`.
    fn aad(table: Table, key: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(AAD_PREFIX.len() + table.name().len() + 1 + key.len());
        out.extend_from_slice(AAD_PREFIX);
        out.extend_from_slice(table.name().as_bytes());
        out.push(0x00);
        out.extend_from_slice(key);
        out
    }

    /// Encrypted put. The plaintext `value` is sealed and stored as
    /// `nonce(12) ‖ ciphertext_with_tag`.
    pub fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<()> {
        let subkey = self.subkey_for(table)?;
        let mut nonce: Nonce = [0u8; NONCE_LEN];
        random::fill(&mut nonce);
        let aad = Self::aad(table, key);
        let ciphertext = aead::seal(ALG, &subkey, &nonce, &aad, value)
            .map_err(|e| StorageError::Cipher(format!("aead seal: {e}")))?;

        let mut wire = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        wire.extend_from_slice(&nonce);
        wire.extend_from_slice(&ciphertext);
        self.inner.put(table, key, &wire)
    }

    /// Encrypted get. Returns `Ok(None)` for missing keys, `Err` for
    /// any tampering / wrong-key / truncation.
    pub fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let Some(wire) = self.inner.get(table, key)? else {
            return Ok(None);
        };
        Some(self.unseal(table, key, &wire)).transpose()
    }

    /// Delete the row. Encryption is irrelevant for a delete — we
    /// just forward to the inner store.
    pub fn delete(&self, table: Table, key: &[u8]) -> Result<()> {
        self.inner.delete(table, key)
    }

    /// Encrypted range. Decrypts every value in the matched range.
    /// A single tampered row aborts the whole call (returns `Err`) —
    /// silent skip would be a security regression (an attacker who
    /// can flip individual rows could hide them from `range`).
    pub fn range(&self, table: Table, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let raw = self.inner.range(table, prefix)?;
        let mut out = Vec::with_capacity(raw.len());
        for (k, wire) in raw {
            let plaintext = self.unseal(table, &k, &wire)?;
            out.push((k, plaintext));
        }
        Ok(out)
    }

    fn unseal(&self, table: Table, key: &[u8], wire: &[u8]) -> Result<Vec<u8>> {
        if wire.len() < NONCE_LEN {
            return Err(StorageError::Cipher(format!(
                "stored value too short (got {} bytes, need at least {NONCE_LEN})",
                wire.len(),
            )));
        }
        let (nonce_bytes, ciphertext) = wire.split_at(NONCE_LEN);
        let mut nonce: Nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(nonce_bytes);
        let subkey = self.subkey_for(table)?;
        let aad = Self::aad(table, key);
        aead::open(ALG, &subkey, &nonce, &aad, ciphertext)
            .map_err(|e| StorageError::Cipher(format!("aead open: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;

    fn fixed_root_key() -> [u8; 32] {
        [0xABu8; 32]
    }

    fn enc() -> EncryptedStore {
        EncryptedStore::new(Arc::new(InMemoryStore::new()), fixed_root_key())
    }

    #[test]
    fn put_get_roundtrip() {
        let store = enc();
        store.put(Table::Messages, b"k", b"hello").unwrap();
        assert_eq!(
            store.get(Table::Messages, b"k").unwrap().as_deref(),
            Some(&b"hello"[..]),
        );
    }

    #[test]
    fn missing_key_returns_none() {
        let store = enc();
        assert!(store.get(Table::Messages, b"missing").unwrap().is_none());
    }

    #[test]
    fn ciphertext_is_not_plaintext() {
        // Confirm the inner store sees encrypted bytes, not the
        // plaintext we put in.
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        let plaintext = b"this is plaintext";
        outer.put(Table::Messages, b"k", plaintext).unwrap();
        let raw = inner.get(Table::Messages, b"k").unwrap().unwrap();
        assert_ne!(raw, plaintext);
        // Nonce(12) + len + tag(16) > plaintext.
        assert!(raw.len() >= NONCE_LEN + plaintext.len() + 16);
    }

    #[test]
    fn tampered_value_fails_open() {
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        outer.put(Table::Messages, b"k", b"hello").unwrap();

        // Flip a bit in the ciphertext (past the nonce).
        let mut raw = inner.get(Table::Messages, b"k").unwrap().unwrap();
        raw[NONCE_LEN] ^= 0x01;
        inner.put(Table::Messages, b"k", &raw).unwrap();

        let err = outer.get(Table::Messages, b"k").unwrap_err();
        assert!(matches!(err, StorageError::Cipher(_)));
    }

    #[test]
    fn moved_row_fails_open() {
        // AAD binds ciphertext to (table, key). Moving the row to a
        // different key — even within the same table — must fail.
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        outer.put(Table::Messages, b"k1", b"hello").unwrap();

        let raw = inner.get(Table::Messages, b"k1").unwrap().unwrap();
        inner.put(Table::Messages, b"k2", &raw).unwrap();

        let err = outer.get(Table::Messages, b"k2").unwrap_err();
        assert!(matches!(err, StorageError::Cipher(_)));
    }

    #[test]
    fn moved_to_other_table_fails_open() {
        // AAD also binds to the table name — and the per-table
        // subkey diverges via HKDF info, so cross-table relocation
        // is doubly broken.
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        outer.put(Table::Messages, b"k", b"hello").unwrap();

        let raw = inner.get(Table::Messages, b"k").unwrap().unwrap();
        inner.put(Table::Contacts, b"k", &raw).unwrap();

        let err = outer.get(Table::Contacts, b"k").unwrap_err();
        assert!(matches!(err, StorageError::Cipher(_)));
    }

    #[test]
    fn wrong_root_key_fails_open() {
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        outer.put(Table::Messages, b"k", b"hello").unwrap();

        let outer2 = EncryptedStore::new(inner, [0xCDu8; 32]);
        let err = outer2.get(Table::Messages, b"k").unwrap_err();
        assert!(matches!(err, StorageError::Cipher(_)));
    }

    #[test]
    fn range_decrypts_every_value() {
        let store = enc();
        store.put(Table::Messages, b"a/0", b"v0").unwrap();
        store.put(Table::Messages, b"a/1", b"v1").unwrap();
        store.put(Table::Messages, b"b/0", b"vX").unwrap();

        let rows = store.range(Table::Messages, b"a/").unwrap();
        let plaintexts: Vec<Vec<u8>> = rows.into_iter().map(|(_, v)| v).collect();
        assert_eq!(plaintexts, vec![b"v0".to_vec(), b"v1".to_vec()]);
    }

    #[test]
    fn nonce_is_random_per_put() {
        // Put the same plaintext twice and confirm the on-wire
        // ciphertext differs (random nonce).
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let outer = EncryptedStore::new(inner.clone(), fixed_root_key());
        outer.put(Table::Messages, b"k", b"value").unwrap();
        let raw1 = inner.get(Table::Messages, b"k").unwrap().unwrap();
        outer.put(Table::Messages, b"k", b"value").unwrap();
        let raw2 = inner.get(Table::Messages, b"k").unwrap().unwrap();
        assert_ne!(raw1, raw2);
    }
}
