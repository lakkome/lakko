// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Anonymous rate-limit tokens ([ADR-0054] decision 2).
//!
//! A privately verifiable token in the Privacy Pass shape, built on a VOPRF
//! (RFC 9497, VOPRF mode, Ristretto255-SHA512):
//!
//! 1. The client picks a random 32-byte nonce per token, builds the input
//!    `token_input(context, epoch, nonce)`, blinds it and sends the blinded
//!    elements to `services/auth` with its session ([`TokenRequest`]).
//! 2. The issuer counts the batch against the account's quota for
//!    `(context, epoch)`, evaluates it under the `(context, epoch)` key and
//!    returns the evaluations plus one batched DLEQ proof ([`IssuerKey::issue`]).
//! 3. The client checks the proof against the published public key for
//!    `(context, epoch)` and unblinds: each result is a [`Token`]
//!    ([`TokenRequest::finalize`]).
//! 4. A consuming service verifies a presented token with the same master
//!    secret ([`verify_token`]) and records its [`SpendId`] in a spent-set
//!    (`lakko-anon-token-server`).
//!
//! The issuer sees an account and a count; the verifier sees a token that
//! some rationed account produced. Blinding makes the two unlinkable.
//!
//! # Keys
//!
//! One master secret ([`MasterSecret`]) shared by the issuer and every
//! verifier; the key for `(context, epoch)` is derived from it
//! ([`IssuerKey::derive`]), so there is no key table and keys rotate every
//! epoch. A verifier accepts the current and the previous epoch. Holding the
//! master lets a service mint tokens - a quota bypass, never a key or content
//! exposure (ADR-0054 decision 2).
//!
//! # Wire
//!
//! ```text
//! token = version(1) || context(1) || epoch(8, BE) || nonce(32) || output(64)
//! ```
//!
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

#![forbid(unsafe_code)]

mod client;
mod context;
mod error;
mod issuer;
mod token;

pub use client::TokenRequest;
pub use context::{epoch_accepted, epoch_at, TokenContext, DEFAULT_WINDOW_SECS};
pub use error::{AnonTokenError, Result};
pub use issuer::{validate_blinded_batch, Issuance, IssuerKey, MasterSecret, MASTER_SECRET_LEN};
pub use token::{token_input, verify_token, SpendId, Token, VerifiedToken, SPEND_ID_LEN};

/// Token wire version.
pub const TOKEN_VERSION: u8 = 1;
/// Length of the client-chosen nonce.
pub const NONCE_LEN: usize = 32;
/// Length of the VOPRF output (SHA-512).
pub const OUTPUT_LEN: usize = 64;
/// Length of an encoded token: version + context + epoch + nonce + output.
pub const TOKEN_LEN: usize = 1 + 1 + 8 + NONCE_LEN + OUTPUT_LEN;
/// Length of a serialized blinded element (a compressed Ristretto255 point).
pub const BLINDED_ELEMENT_LEN: usize = 32;
/// Length of a serialized evaluated element.
pub const EVALUATION_ELEMENT_LEN: usize = 32;
/// Length of a serialized DLEQ proof (two scalars).
pub const PROOF_LEN: usize = 64;
/// Length of a serialized issuer public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// The largest batch the issuer evaluates in one request, and the largest a
/// client asks for. Bounds the per-request work and the DLEQ proof input.
pub const MAX_BATCH: usize = 32;

/// Fuzzing entry points. Not part of the supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{verify_token, MasterSecret, Token, TokenContext};

    /// Fixed master secret for the fuzz harness.
    const FUZZ_MASTER: [u8; 32] = [0x5A; 32];
    /// Fixed clock: epoch 20_000 under the default window.
    const FUZZ_NOW: u64 = 20_000 * super::DEFAULT_WINDOW_SECS;

    /// Decode the bytes as a token and, for every context, run the full
    /// verifier on them under a fixed master and clock. Asserts that a token
    /// which decodes re-encodes to the identical bytes. Returns whether the
    /// bytes decoded.
    #[must_use]
    pub fn decode_and_verify(data: &[u8]) -> bool {
        let master = MasterSecret::from_bytes(FUZZ_MASTER);
        for ctx in TokenContext::ALL {
            let _ = verify_token(&master, data, ctx, FUZZ_NOW, super::DEFAULT_WINDOW_SECS);
        }
        match Token::decode(data) {
            Ok(t) => {
                assert_eq!(&t.encode()[..], data, "decode/encode is not canonical");
                true
            },
            Err(_) => false,
        }
    }
}
