// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS (RFC 9420) wrapper for Lakko.
//!
//! Wraps OpenMLS to fix the supported ciphersuites, isolate upstream API
//! churn and expose Lakko-specific abstractions:
//!
//! - [`identity::Identity`] — signing keypair + credential + per-device
//!   storage / crypto provider
//! - [`group::Group`] — single MLS conversation group with create / join /
//!   add / remove / message / process / exporter operations
//!
//! See `docs/adr/0001-mls-only.md`.

#![forbid(unsafe_code)]

pub mod ciphersuite;
pub mod error;
pub mod group;
pub mod identity;
pub mod key_package;

pub use error::{Error, Result};
pub use group::{Group, Incoming, MemberAddition, MemberRecord};
pub use identity::Identity;

/// Re-exported MLS leaf-node index. Surfaced so callers do not need to
/// depend on `openmls` directly.
pub use openmls::prelude::LeafNodeIndex;

/// Parser entry points for fuzz harnesses.
///
/// These are **not** part of the supported public API. They exist so
/// `cargo fuzz` targets in `crates/lakko-mls/fuzz/` can drive bytes through
/// the wire-format parsers without setting up real Identity / Group state.
/// Each function is panic-safe by contract — fuzzing failures should
/// surface as `false` (parse rejected), never as a panic.
#[doc(hidden)]
pub mod fuzz_api {
    use openmls::framing::MlsMessageIn;
    use tls_codec::Deserialize as _;

    /// Try to parse a wire-format `MlsMessageIn`. Returns `true` on success.
    #[must_use]
    pub fn parse_mls_message(bytes: &[u8]) -> bool {
        let mut slice = bytes;
        MlsMessageIn::tls_deserialize(&mut slice).is_ok()
    }

    /// Try to parse and validate a wire-format `KeyPackage`. Returns `true`
    /// on successful TLS-codec parse.
    #[must_use]
    pub fn parse_key_package(bytes: &[u8]) -> bool {
        crate::key_package::deserialize(bytes).is_ok()
    }
}
