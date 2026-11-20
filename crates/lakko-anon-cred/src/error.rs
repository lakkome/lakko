// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for the anonymous credential.

/// Every way a credential operation can refuse.
///
/// The variants carry no bytes of the input, no key and no secret, so an error
/// can be logged without leaking anything a later request could correlate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnonCredError {
    /// The input does not have the exact length its layout requires.
    #[error("anonymous credential input has the wrong length")]
    Malformed,
    /// The version byte is not the one this implementation reads.
    #[error("anonymous credential input has an unknown version")]
    UnknownVersion,
    /// A 32-byte field is not the canonical encoding of a Ristretto255 point
    /// or of a scalar below the group order.
    #[error("non-canonical group element or scalar")]
    NonCanonical,
    /// A point that must not be the identity is the identity.
    #[error("identity point where a non-identity point is required")]
    IdentityPoint,
    /// The credential period is neither the current nor the previous one.
    #[error("credential period is outside the accepted window")]
    PeriodNotAccepted,
    /// The issuance response names another period than the request.
    #[error("issuance response is for another period")]
    WrongPeriod,
    /// A zero-knowledge proof does not verify.
    #[error("proof does not verify")]
    BadProof,
    /// The bound message is longer than [`crate::MAX_MESSAGE_LEN`].
    #[error("bound message is too long")]
    MessageTooLong,
    /// The master secret is not 32 bytes of hex.
    #[error("master secret must be 64 hex characters (32 bytes)")]
    InvalidMasterSecret,
    /// A derivation produced the zero scalar (probability about 2^-252).
    #[error("derived scalar is zero")]
    ZeroScalar,
    /// The operating system's random number generator failed.
    #[error("randomness unavailable")]
    RandomnessUnavailable,
    /// The prover's own witness does not satisfy the statement it was asked
    /// to prove. Reached only by a caller bug, never by peer input; surfaced
    /// rather than producing a proof that would not verify.
    #[error("witness does not satisfy the statement")]
    Internal,
}

impl AnonCredError {
    /// A stable kebab-case code for the variant, the same on every FFI
    /// surface and in every service response.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownVersion => "unknown-version",
            Self::NonCanonical => "non-canonical",
            Self::IdentityPoint => "identity-point",
            Self::PeriodNotAccepted => "period-not-accepted",
            Self::WrongPeriod => "wrong-period",
            Self::BadProof => "bad-proof",
            Self::MessageTooLong => "message-too-long",
            Self::InvalidMasterSecret => "invalid-master-secret",
            Self::ZeroScalar => "zero-scalar",
            Self::RandomnessUnavailable => "randomness-unavailable",
            Self::Internal => "internal",
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = core::result::Result<T, AnonCredError>;

#[cfg(test)]
mod tests {
    use super::AnonCredError as E;

    #[test]
    fn codes_are_distinct_and_frozen() {
        let all = [
            E::Malformed,
            E::UnknownVersion,
            E::NonCanonical,
            E::IdentityPoint,
            E::PeriodNotAccepted,
            E::WrongPeriod,
            E::BadProof,
            E::MessageTooLong,
            E::InvalidMasterSecret,
            E::ZeroScalar,
            E::RandomnessUnavailable,
            E::Internal,
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert_eq!(E::BadProof.code(), "bad-proof");
        assert_eq!(E::PeriodNotAccepted.code(), "period-not-accepted");
    }
}
