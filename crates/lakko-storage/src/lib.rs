// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Local encrypted storage for Lakko clients.
//!
//! ## Architecture (three layers)
//!
//! ```text
//! +------------------------+   high-level domain types: messages,
//! | MessageStore           |   group state, contacts, settings.
//! | GroupStateStore        |   What the rest of the client uses.
//! +------------------------+
//!            |
//!            v
//! +------------------------+   AEAD per value, per-table subkey,
//! | EncryptedStore         |   AAD bound to (table, key).
//! +------------------------+
//!            |
//!            v
//! +------------------------+   raw KV. SqliteStore (rusqlite,
//! | Store trait            |   bundled libsqlite) for production,
//! | - InMemoryStore        |   InMemoryStore for tests.
//! | - SqliteStore          |
//! +------------------------+
//! ```
//!
//! ## Cipher choice
//!
//! Application-layer ChaCha20-Poly1305, NOT SQLCipher. Rationale:
//! the values we store are already opaque to outsiders (encrypted
//! MLS state, application messages received post-decryption,
//! pseudonym-keyed records); the row keys themselves carry no
//! plaintext (they are pseudonyms, hashes, UUIDs). App-layer AEAD
//! gives the same effective property as SQLCipher's full-page
//! encryption for our threat model, without dragging OpenSSL into
//! the cross-compile story for iOS / Android / Tauri targets.
//!
//! ## Hardware-backed root key
//!
//! [`EncryptedStore::new`] takes a `[u8; 32]` root_key parameter.
//! Phase 1.7 leaves the *source* of that key to the caller. Phase
//! 1.13–1.16 wires up per-platform secure-element integrations:
//!
//!   * iOS / iPadOS / macOS / watchOS — Secure Enclave via CryptoKit
//!   * Android — StrongBox / Android Keystore
//!   * Linux — TPM 2.0 via tpm2-tss
//!   * Windows — NCrypt / TPM
//!   * Web — non-extractable WebCrypto
//!
//! The contract is: the caller derives a 32-byte key from a
//! non-extractable hardware key (via HKDF or platform-native KDF)
//! and hands that derived key to `EncryptedStore`. The hardware key
//! itself never enters Rust memory.
//!
//! ## Phase-1.7 deferrals
//!
//! * **IndexedDB backend** for Web — needs `wasm-bindgen` and is part
//!   of the `lakko-ffi-wasm` sprint (1.13).
//! * **SQLCipher full-DB encryption** — Phase 2 hardening if a
//!   concrete attacker model demands it (e.g. forensic recovery
//!   from a wiped device with cold-storage attacker).
//! * **Local schema migrations** — Phase 2; for now schema bumps
//!   require a fresh DB.
//! * **Drafts / settings / advanced UI state** — added when the UI
//!   sprints (1.14–1.16) call for them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cipher;
mod error;
mod groups;
mod messages;
mod store;

pub use cipher::EncryptedStore;
pub use error::{Result, StorageError};
pub use groups::GroupStateStore;
pub use messages::{LocalMessage, MessageKind, MessageStore};
pub use store::{InMemoryStore, SqliteStore, Store, Table};
