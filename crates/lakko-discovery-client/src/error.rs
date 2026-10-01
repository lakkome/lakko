// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-discovery-client`.

use thiserror::Error;

/// Failure modes for the OPRF client flows.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DiscoveryClientError {
    /// Caller passed a base64 string that does not decode — wire
    /// corruption or version skew, not user input.
    #[error("invalid base64 payload: {0}")]
    Base64(String),

    /// Bytes decode but do not deserialise as the expected
    /// `voprf` wire structure (e.g. the server returned a
    /// `BlindedElement` where the client expected an
    /// `EvaluationElement`). Indicates a wiring mistake.
    #[error("invalid VOPRF wire payload")]
    InvalidWire,

    /// `voprf` rejected the math — e.g. malformed group element,
    /// failed proof verification (in VOPRF mode). Treated as a
    /// single opaque failure; finer detail does not help the UI.
    #[error("VOPRF protocol failure")]
    Voprf,

    /// Caller passed an empty or whitespace-only username after
    /// normalisation — surfaced before the network round-trip
    /// to make form-validation messages clean.
    #[error("username is empty after normalisation")]
    EmptyUsername,

    /// The typed username fails the ADR-0048 §4 character-set rule
    /// (after normalisation: `[a-z0-9_]`, 3 to 32 characters). Raised by
    /// [`crate::OprfBlinder::blind`] before any network round-trip, so a
    /// non-conforming name is neither registered nor looked up.
    ///
    /// Carries a reason code only — never the offending character, since
    /// the input is a lookup target.
    #[error("invalid username: {0}")]
    InvalidUsername(#[from] crate::UsernameError),

    /// A name query is not searchable (over 1024 bytes, or under 2
    /// characters after [`crate::normalize_name_for_search`]), or a string
    /// passed as a name term is not one ([`crate::is_name_term`]); ADR-0060
    /// section 5. Raised by [`crate::OprfBlinder::blind_name`] and
    /// [`crate::OprfBlinder::blind_name_term`] before any network round-trip.
    /// Carries no part of the input.
    #[error("display name is not searchable")]
    InvalidName,
}

impl DiscoveryClientError {
    pub(crate) fn base64<E: core::fmt::Display>(err: E) -> Self {
        Self::Base64(err.to_string())
    }
    pub(crate) fn invalid_wire() -> Self {
        Self::InvalidWire
    }
    pub(crate) fn voprf() -> Self {
        Self::Voprf
    }
}
