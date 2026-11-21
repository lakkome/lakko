// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Crate-wide error type.
//!
//! `lakko-channels` errors are deliberately coarse — every distinct
//! failure path matters for security analysis, so we surface them as
//! distinct variants rather than collapsing into a single
//! `InvalidPost`. Callers map to user-visible messages at the FFI
//! boundary.

use thiserror::Error;

/// `Result<T, ChannelError>` alias for crate consumers.
pub type Result<T> = std::result::Result<T, ChannelError>;

/// All failure modes exposed by `lakko-channels`.
#[derive(Debug, Error)]
pub enum ChannelError {
    /// The MLS exporter returned a buffer of the wrong length.
    ///
    /// The caller is contractually obliged to pass exactly 32 bytes;
    /// this guards against a callsite that swaps the `length`
    /// argument to `Group::export_secret`.
    #[error("expected {expected}-byte seed, got {got}")]
    SeedLength {
        /// Required seed length in bytes (always 32).
        expected: usize,
        /// Buffer length the caller actually passed.
        got: usize,
    },

    /// HKDF derivation failed. Cannot happen with the fixed
    /// per-message key / nonce lengths we use, but surfaced for
    /// completeness so a future refactor that bumps lengths beyond
    /// `HKDF_SHA256_MAX_OUT` cannot silently degrade.
    #[error("HKDF derivation failed")]
    KeyDerivation,

    /// AEAD seal failed. Effectively impossible with valid inputs
    /// because ChaCha20-Poly1305 has no failure modes other than
    /// "exceeds 2^32 − 1 blocks", which our `MAX_PLAINTEXT_LEN`
    /// guard rules out.
    #[error("AEAD seal failed")]
    AeadSeal,

    /// AEAD open failed — either the seed is wrong, the AAD does
    /// not match what the publisher committed to, or the
    /// ciphertext was tampered with.
    #[error("AEAD open failed (wrong seed, AAD mismatch, or tampered ciphertext)")]
    AeadOpen,

    /// Ed25519 signature did not verify against the post's
    /// `sender_pubkey`.
    #[error("Ed25519 signature verification failed")]
    BadSignature,

    /// Plaintext exceeds `MAX_PLAINTEXT_LEN`. The cap is generous
    /// (16 MiB by default) but bounded so we never approach the
    /// ChaCha20 counter-overflow threshold.
    #[error("plaintext exceeds {max} bytes (got {got})")]
    PlaintextTooLong {
        /// Configured maximum.
        max: usize,
        /// Actual length the caller passed.
        got: usize,
    },

    /// A member card's profile exceeds `MAX_MEMBER_CARD_PROFILE_LEN`
    /// (ADR-0060).
    #[error("member card profile exceeds {max} bytes (got {got})")]
    MemberCardTooLong {
        /// Configured maximum.
        max: usize,
        /// Actual length the caller passed.
        got: usize,
    },

    /// Wire format failed to parse — short buffer, truncated field,
    /// or invalid length prefix.
    #[error("wire format invalid: {0}")]
    Wire(&'static str),

    /// `sender_pubkey` failed to parse as Ed25519 (wrong length, or
    /// an Edwards-curve point that fails the public-key validation
    /// in the verifier crate).
    #[error("sender public key is not a valid Ed25519 verifying key")]
    BadSenderKey,

    /// A subscriber key is not a valid Ed25519 verifying key (wrong
    /// length, not a curve point) or is of small order (ADR-0060: the
    /// subscriber key is Ed25519, its X25519 seed-envelope key derived).
    #[error("subscriber key is not a valid Ed25519 key of full order")]
    BadSubscriberKey,

    /// A member card opened but its account binding did not verify: the
    /// identity key is not a valid Ed25519 key of full order, or its
    /// signature does not cover this channel, subscriber key and profile
    /// (ADR-0060 section 3). The card is dropped, never shown.
    #[error("member card is not signed by the account it names")]
    MemberCardBinding,

    /// A signed unsubscribe request is outside its freshness window
    /// (ADR-0060 section 3, [`crate::unsubscribe`]).
    #[error("unsubscribe request is outside its freshness window")]
    StaleRequest,

    /// A reaction emoji fails the chat reaction rule
    /// (`lakko_attachments::is_reaction_emoji`: empty, or longer than 64
    /// bytes; ADR-0087 section 13a).
    #[error("emoji is not a valid reaction")]
    InvalidReactionEmoji,
}

impl From<lakko_crypto::error::Error> for ChannelError {
    fn from(value: lakko_crypto::error::Error) -> Self {
        // The crypto crate deliberately collapses all AEAD failures
        // into a single `Aead` variant to avoid leaking detail. We
        // mirror that and surface the right channel-level variant
        // based on the call site that wraps the conversion: the
        // direct mappings here cover the strictly-defined paths.
        use lakko_crypto::error::Error as C;
        match value {
            C::HkdfTooLong(_) => Self::KeyDerivation,
            C::Aead => Self::AeadOpen,
            C::SignatureVerification => Self::BadSignature,
            C::InvalidLength { .. } | C::PasswordHashing | C::NonContributory => {
                Self::Wire("crypto-layer rejection")
            },
        }
    }
}
