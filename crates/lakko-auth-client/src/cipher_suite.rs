// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE cipher suite definition — verbatim mirror of the
//! server-side type in `services/auth/src/opaque.rs`.
//!
//! Both halves of the protocol must instantiate the same
//! `CipherSuite` impl; every byte on the wire (`RegistrationRequest`,
//! `RegistrationResponse`, `CredentialRequest`,
//! `CredentialFinalization`) is generic over this trait, and a
//! mismatch will produce "invalid credentials" with no recoverable
//! diagnostic at runtime.
//!
//! Whenever the server's `LakkoCipherSuite` changes, this file
//! changes in the same commit.

use opaque_ke::key_exchange::tripledh::TripleDh;
use opaque_ke::{CipherSuite, Ristretto255};

/// Lakko OPAQUE-PAKE cipher suite.
///
/// - **OPRF group**: Ristretto255 (constant-time, software, no
///   hardware deps).
/// - **KE group**: Ristretto255 (matches OPRF for TripleDH).
/// - **KE**: TripleDH (3-message authenticated key exchange,
///   RFC draft `opaque` §6.4.1).
/// - **KSF**: Argon2id with the library default parameters
///   (client-side stretching; defends the password against
///   offline brute-force even if the server-side state leaks).
///
/// MUST equal the server-side `LakkoCipherSuite` byte-for-byte.
/// See `services/auth/src/opaque.rs` for the canonical definition;
/// this crate's tests assert byte-identical behaviour by running a
/// full registration + login round-trip against the very same
/// `opaque-ke` server primitives.
pub struct LakkoCipherSuite;

impl CipherSuite for LakkoCipherSuite {
    type OprfCs = Ristretto255;
    type KeGroup = Ristretto255;
    type KeyExchange = TripleDh;
    type Ksf = argon2::Argon2<'static>;
}

/// Length in bytes of the OPAQUE `export_key` for this suite.
///
/// `export_key` is the output of HKDF-Expand inside OPAQUE bound to
/// the OPRF hash length; for Ristretto255 (OPRF over SHA-512) that
/// is **64 bytes**. The constant is the source of truth — if the
/// underlying suite changes, change this here and the client glue
/// will update accordingly.
///
/// Used by callers to size the buffer that receives the export key
/// (e.g. before deriving a device root key).
pub const EXPORT_KEY_LEN: usize = 64;

/// Length in bytes of the OPAQUE `session_key` produced at the end
/// of a successful login. Same provenance as `EXPORT_KEY_LEN` —
/// 64 bytes for Ristretto255-SHA512.
pub const SESSION_KEY_LEN: usize = 64;
