// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Cryptographic primitives for Lakko.
//!
//! Wraps audited primitives (HPKE, HKDF, AEAD, BLAKE3, Argon2id, Ed25519,
//! X25519, OPRF) behind a stable internal API. No `unsafe`. All key material
//! that crosses crate boundaries is wrapped in `Zeroize`-on-drop containers.
//!
//! See `docs/adr/0001-mls-only.md` for ciphersuite decisions.

#![forbid(unsafe_code)]
