// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Cryptographic primitives for Lakko.
//!
//! Thin, audit-friendly wrappers over the RustCrypto and Dalek primitives:
//!
//! - [`hash`] — BLAKE3 (default), SHA-256 (interop)
//! - [`kdf`] — HKDF-SHA-256, Argon2id
//! - [`aead`] — ChaCha20-Poly1305, AES-256-GCM
//! - [`signing`] — Ed25519
//! - [`agreement`] — X25519 Diffie-Hellman
//! - [`secret`] — `Zeroize`-on-drop secret container
//! - [`random`] — OS RNG access
//! - [`error`] — common [`Error`] / [`Result`] types
//!
//! No `unsafe`. All key material that crosses crate boundaries goes through
//! [`secret::SecretBytes`].
//!
//! See `docs/adr/0001-mls-only.md` for ciphersuite decisions.

#![forbid(unsafe_code)]

pub mod aead;
pub mod agreement;
pub mod error;
pub mod hash;
pub mod kdf;
pub mod random;
pub mod secret;
pub mod signing;

pub use error::{Error, Result};
pub use secret::SecretBytes;
