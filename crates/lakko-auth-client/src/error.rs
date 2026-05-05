// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-auth-client`.
//!
//! Every fallible operation in the crate funnels through
//! [`AuthClientError`]. We intentionally lose the rich enum shape
//! that `opaque-ke` exposes for `ProtocolError` because:
//!
//!   1. The OPAQUE design philosophy is "indistinguishable failure"
//!      — leaking why a login failed (wrong password vs unknown
//!      account vs corrupted envelope) erodes the security guarantee.
//!   2. UI-visible errors are a small alphabet ("network /
//!      bad input / wrong credentials") and a structured client
//!      error doesn't need finer granularity than that.
//!
//! Rich diagnostic data is still available via [`tracing`] in
//! debug builds — see the `Debug` impl on the source type when
//! collapsed into [`Opaque`](AuthClientError::Opaque).

use thiserror::Error;

/// Result alias for ergonomics inside the crate.
pub(crate) type Result<T> = core::result::Result<T, AuthClientError>;

/// Failure modes for the OPAQUE-PAKE client flows.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuthClientError {
    /// Caller passed a base64 string that doesn't decode — usually
    /// a wire-corruption or version-skew bug, not user input.
    #[error("invalid base64 payload: {0}")]
    Base64(String),

    /// Caller passed bytes that don't deserialize into the expected
    /// OPAQUE wire structure — e.g. a `RegistrationResponse` that
    /// is actually a `CredentialResponse`. Indicates a wiring
    /// mistake at the call site.
    #[error("invalid OPAQUE wire payload")]
    InvalidWire,

    /// `opaque-ke` rejected the math. For login, this is the path
    /// taken on a wrong password / unknown account / replayed
    /// envelope; do **not** surface a finer error to the UI.
    #[error("OPAQUE protocol failure")]
    Opaque,
}

impl AuthClientError {
    /// Build a `Base64` variant — internal helper used by every
    /// decoder in the crate.
    pub(crate) fn base64<E: core::fmt::Display>(err: E) -> Self {
        Self::Base64(err.to_string())
    }

    /// Build an `InvalidWire` variant. Kept as a constructor (not a
    /// `From` impl) because the underlying `opaque-ke` types vary
    /// per call site and we don't want them to leak into our
    /// public API.
    pub(crate) fn invalid_wire() -> Self {
        Self::InvalidWire
    }

    /// Build an `Opaque` variant for any `opaque-ke` `ProtocolError`.
    pub(crate) fn opaque() -> Self {
        Self::Opaque
    }
}
