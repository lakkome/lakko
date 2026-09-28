// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Errors surfaced by `lakko-contacts`.

use thiserror::Error;

/// One enum for the whole crate. Discriminants:
///
///   * **Crypto** — primitive failure (AEAD seal/open, signing,
///     KDF, key parsing). Wraps `lakko_crypto::error::Error`.
///   * **InvalidEncoding** — wire-format codec rejected the input
///     (bad length, unknown tag, trailing bytes, …).
///   * **BadSignature** — the Ed25519 signature on a `ContactRequest`
///     does not match the claimed sender key.
///   * **WrongRecipient** — the sealed envelope was not addressed to
///     us (decryption failed because we used the wrong recipient
///     key). Distinct from `Crypto::Aead` so callers can distinguish
///     "tampered" from "not for me".
///   * **InvalidArgument** — caller passed nonsense (e.g. zero-length
///     username).
#[derive(Error, Debug)]
pub enum ContactError {
    /// A `lakko-crypto` primitive returned an error.
    #[error("crypto primitive failure")]
    Crypto(#[source] lakko_crypto::error::Error),

    /// Wire format rejected the input.
    #[error("invalid encoding: {0}")]
    InvalidEncoding(String),

    /// The signature on a `ContactRequest` did not verify against the
    /// claimed sender public key.
    #[error("contact request signature did not verify")]
    BadSignature,

    /// Envelope decryption failed — most likely because the envelope
    /// was sealed for a different recipient. Treat as "not for me",
    /// silently drop and move on.
    #[error("sealed envelope is not addressed to this recipient")]
    WrongRecipient,

    /// Caller-side argument validation.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A sealed invite link (`/i/`, `/c/`, `/g/`) did not open: a missing or
    /// wrong link secret, a blob of another kind, a truncated, tampered or
    /// swapped blob, or a capability that does not verify inside it. One
    /// variant on purpose — the page says "this link is damaged" for all of
    /// them (see [`crate::link_seal`]).
    #[error("invite link is damaged")]
    LinkDamaged,
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ContactError>;

impl From<lakko_crypto::error::Error> for ContactError {
    fn from(e: lakko_crypto::error::Error) -> Self {
        ContactError::Crypto(e)
    }
}
