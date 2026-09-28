// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for the anonymous-token primitive.

/// Every way an anonymous-token operation can refuse.
///
/// The variants never carry the token bytes, a key or a nonce, so an error can
/// be logged without leaking anything a later request could correlate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnonTokenError {
    /// The token is not exactly [`crate::TOKEN_LEN`] bytes.
    #[error("anonymous token has the wrong length")]
    Malformed,
    /// The token's version byte is not [`crate::TOKEN_VERSION`].
    #[error("anonymous token has an unknown version")]
    UnknownVersion,
    /// A context byte or name that is not a [`crate::TokenContext`].
    #[error("unknown anonymous-token context")]
    UnknownContext,
    /// The token names a different context than the verifier expects.
    #[error("anonymous token was issued for another context")]
    WrongContext,
    /// The token's epoch is neither the current nor the previous one.
    #[error("anonymous token epoch is outside the accepted window")]
    EpochOutOfWindow,
    /// The recomputed VOPRF output does not match the token's output.
    #[error("anonymous token does not verify")]
    InvalidToken,
    /// A batch of zero or more than [`crate::MAX_BATCH`] elements.
    #[error("batch size must be between 1 and MAX_BATCH")]
    BadBatchSize,
    /// A blinded or evaluated element that is not a valid, non-identity
    /// group element (or the wrong length).
    #[error("invalid group element")]
    BadElement,
    /// The issuer's public key is not a valid, non-identity group element.
    #[error("invalid issuer public key")]
    BadPublicKey,
    /// The DLEQ proof is malformed or does not verify under the issuer's
    /// public key: the issuer did not evaluate under the key it claims.
    #[error("issuer proof does not verify")]
    ProofVerification,
    /// The number of evaluations does not match the number of blinded
    /// elements the client sent.
    #[error("evaluation count does not match the request")]
    CountMismatch,
    /// The epoch window is zero seconds.
    #[error("epoch window must be at least one second")]
    InvalidWindow,
    /// The master secret is not 32 bytes of hex.
    #[error("master secret must be 64 hex characters (32 bytes)")]
    InvalidMasterSecret,
    /// An internal VOPRF failure that valid inputs cannot reach (key
    /// derivation or proof generation). Surfaced rather than panicking.
    #[error("internal VOPRF failure")]
    Internal,
}

impl AnonTokenError {
    /// A stable kebab-case code for the variant, the same on every FFI
    /// surface, so a client branches on it rather than on the prose.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownVersion => "unknown-version",
            Self::UnknownContext => "unknown-context",
            Self::WrongContext => "wrong-context",
            Self::EpochOutOfWindow => "epoch-out-of-window",
            Self::InvalidToken => "invalid-token",
            Self::BadBatchSize => "bad-batch-size",
            Self::BadElement => "bad-element",
            Self::BadPublicKey => "bad-public-key",
            Self::ProofVerification => "proof-verification",
            Self::CountMismatch => "count-mismatch",
            Self::InvalidWindow => "invalid-window",
            Self::InvalidMasterSecret => "invalid-master-secret",
            Self::Internal => "internal",
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = core::result::Result<T, AnonTokenError>;

#[cfg(test)]
mod tests {
    use super::AnonTokenError as E;

    #[test]
    fn codes_are_distinct_and_frozen() {
        let all = [
            E::Malformed,
            E::UnknownVersion,
            E::UnknownContext,
            E::WrongContext,
            E::EpochOutOfWindow,
            E::InvalidToken,
            E::BadBatchSize,
            E::BadElement,
            E::BadPublicKey,
            E::ProofVerification,
            E::CountMismatch,
            E::InvalidWindow,
            E::InvalidMasterSecret,
            E::Internal,
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert_eq!(E::ProofVerification.code(), "proof-verification");
        assert_eq!(E::BadBatchSize.code(), "bad-batch-size");
    }
}
