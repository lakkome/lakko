// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Encrypted-backup envelope for Phase 2a1.
//!
//! Wraps a serialised [`payload::BackupPayload`] in a wire-level
//! AEAD envelope keyed by an Argon2id-derived KEK from the user's
//! BIP39 recovery phrase. The envelope is what a client uploads
//! to `services/storage` for safekeeping; restoring on a fresh
//! device requires only the recovery phrase to re-derive the KEK
//! and decrypt the blob.
//!
//! ## What's here (Phase 2a1)
//!
//! - [`envelope`] — the on-the-wire envelope codec:
//!   `(version, salt, nonce, ciphertext_len, ciphertext)`. Fixed
//!   Argon2id parameters per `SCHEMA_VERSION` so two clients
//!   produce byte-identical envelopes for the same `(phrase,
//!   salt, nonce, payload)` tuple. AEAD = ChaCha20-Poly1305 with
//!   the AAD binding the schema version + salt + nonce so a
//!   downgrade or salt-tamper invalidates the tag.
//! - [`payload`] — the wire shape carried inside the envelope.
//!   Phase 2a1 minimum: `(account_id, created_at_ms,
//!   ed25519_seed, accepted_contacts)`. DeviceGroup membership
//!   and per-group MLS snapshots land alongside as 2a3 / 2b
//!   come online; the payload's wire format is versioned so the
//!   field set grows backwards-compatibly.
//!
//! ## What this is NOT
//!
//! - The transport / storage layer. The envelope just gives you
//!   bytes; the client posts those to `services/storage` and the
//!   server treats them as an opaque blob.
//! - A message-history backup. Message history flows through the
//!   DeviceGroup catch-up bundle path (2a3d), not through the
//!   backup envelope. The envelope is for "identity + structural
//!   state" — small, frequent-write, easy-restore.
//! - A multi-device sync primitive. Backup is for the
//!   "fresh device, only the recovery phrase, restore me to a
//!   working baseline" case. Cross-device live sync is 2a3.
//!
//! ## Threat-model notes
//!
//! - The KEK derivation runs Argon2id with OWASP-recommended
//!   interactive parameters (m=64 MiB, t=3, p=4) — same shape
//!   `lakko-crypto::kdf::argon2id_kek` already uses for the
//!   recovery-phrase paths elsewhere in the stack. An attacker
//!   who steals the encrypted blob without the phrase is faced
//!   with the Argon2id work factor before they can even attempt
//!   a guess.
//! - The salt is 16 bytes from the CSPRNG, generated fresh per
//!   upload. Re-uploads (e.g. daily snapshots) get distinct
//!   ciphertexts even when the underlying payload doesn't
//!   change — keeps the server from learning "this user's state
//!   didn't change today".
//! - AAD binds the ciphertext to the schema version + salt +
//!   nonce. A tampered `salt` or version byte fails the AEAD tag
//!   check rather than silently decrypting under a different key.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod envelope;
pub mod error;
pub mod payload;

pub use envelope::{open, seal, ENVELOPE_DOMAIN_V1, SALT_LEN, SCHEMA_VERSION};
pub use error::{BackupError, Result};
pub use payload::{AcceptedContactRecord, BackupPayload, PAYLOAD_VERSION};
