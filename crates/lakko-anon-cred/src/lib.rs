// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Keyed-verification anonymous credential for channel polls ([ADR-0092]
//! section 4): one account, one vote per poll, without the verifying service
//! learning the account.
//!
//! The construction is the algebraic MAC `MAC_GGM` of Chase, Meiklejohn and
//! Zaverucha (CMZ14, "Algebraic MACs and Keyed-Verification Anonymous
//! Credentials", CCS 2014) on Ristretto255, with two scalar attributes: the
//! account's hidden credential secret `s` and the public period `p`.
//!
//! 1. **Registration** (client -> auth, once per account and after a key
//!    reset): the commitment `C = s*G` with a proof of knowledge of `s`
//!    ([`CredentialSecret::register`], [`verify_registration`]).
//! 2. **Issuance** (client -> auth, once per period): a proof of knowledge of
//!    the `s` behind the stored `C`, bound to the period's public key
//!    ([`CredentialSecret::issuance_request`]); auth answers with a MAC
//!    `(U, U')`, `U' = (x0 + x1*s + x2*p)*U`, computed from `C` without `s`,
//!    and a proof that it used the published key ([`IssuerSecretKey::issue`]);
//!    the client checks it ([`CredentialSecret::finalize_issuance`]).
//! 3. **Presentation** (client -> channels, per vote): a re-randomised,
//!    committed form of the MAC, the tag `T = s*H_poll`, and a proof that the
//!    credential is valid for the presented period and that the same `s` is
//!    its attribute and the discrete log of `T`, Fiat-Shamir-bound to the
//!    poll and the request content ([`CredentialSecret::present`],
//!    [`CredentialVerifier::verify`]).
//!
//! # Deviation from the ADR text, recorded for the review
//!
//! ADR-0092 4.2 describes the issuance request as "`s` encrypted to itself
//! (ElGamal under a fresh client key) with a proof that the encrypted value is
//! the discrete log of the stored `C`", following CMZ14's blind issuance. In
//! CMZ14 that ciphertext encrypts `s*G` so the issuer can compute the MAC
//! homomorphically without seeing it. Here the issuer already stores exactly
//! `C = s*G`, so the ciphertext would encrypt a value the issuer holds and
//! hide nothing. This crate therefore sends only the proof of knowledge of
//! `s` behind `C`, and the issuer computes the same MAC CMZ14's blind issuance
//! yields (`U = b*G`, `s*U = b*C`) in the clear. The properties the ADR asks
//! for hold: auth never sees `s`, a request for another `s` than `C`'s is
//! refused, and a presentation is unlinkable to its issuance because it is
//! re-randomised. Every other step follows CMZ14 section 4.
//!
//! # Keys
//!
//! One [`MasterSecret`] shared by the issuer (`services/auth`) and the
//! verifier (`services/channels`), separate from the anonymous-token master;
//! each period's key is derived from it ([`IssuerSecretKey::derive`]). Holding
//! the master lets a service mint credentials (ADR-0092 4.4).
//!
//! [ADR-0092]: ../../../docs/adr/0092-attachment-kinds.md

#![forbid(unsafe_code)]

mod client;
mod error;
mod group;
mod issuer;
mod key;
mod proof;
mod relations;
mod transcript;
mod verifier;

pub use client::{
    Commitment, CommitmentRegistration, Credential, CredentialSecret, IssuanceRequest,
    COMMITMENT_LEN, CREDENTIAL_LEN, ISSUANCE_REQUEST_LEN, REGISTRATION_LEN, SECRET_DERIVE_CONTEXT,
};
pub use error::{AnonCredError, Result};
pub use issuer::{verify_registration, IssuanceResponse, ISSUANCE_RESPONSE_LEN};
pub use key::{
    period_accepted, period_at, IssuerPublicKey, IssuerSecretKey, MasterSecret, MASTER_SECRET_LEN,
    PERIOD_SECS, PUBLIC_KEY_LEN,
};
pub use verifier::{
    CredentialVerifier, PollScope, Presentation, VerifiedPresentation, PRESENTATION_LEN, SCOPE_LEN,
    TAG_LEN,
};

/// Version byte of every wire form in this crate.
pub const WIRE_VERSION: u8 = 1;

/// The longest message a presentation may be bound to. Chosen, not measured:
/// a vote carries at most ten 16-byte labels (ADR-0092 4.3) and an action.
pub const MAX_MESSAGE_LEN: usize = 1024;

/// Entry points for the fuzz harnesses in `fuzz/`. Not a supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use crate::{
        Commitment, CommitmentRegistration, Credential, CredentialVerifier, IssuanceRequest,
        IssuanceResponse, IssuerSecretKey, MasterSecret, PollScope, Presentation,
    };

    /// Fixed master secret of the harnesses.
    pub const FUZZ_MASTER: [u8; 32] = [7u8; 32];
    /// Fixed current period of the harnesses.
    pub const FUZZ_PERIOD: u64 = 700;

    /// Decode a stored credential; anything that decodes must re-encode to the
    /// identical bytes.
    pub fn credential_decode(data: &[u8]) {
        if let Ok(credential) = Credential::decode(data) {
            assert_eq!(credential.to_bytes().as_slice(), data);
        }
    }

    /// Decode as an issuance request, an issuance response and a commitment
    /// registration; whatever decodes must re-encode to the identical bytes,
    /// and no registration or request the fuzzer built may verify (it would
    /// need a discrete log).
    pub fn issuance_request_decode(data: &[u8]) {
        if let Ok(request) = IssuanceRequest::decode(data) {
            assert_eq!(request.to_bytes(), data);
            let Ok(key) =
                IssuerSecretKey::derive(&MasterSecret::from_bytes(FUZZ_MASTER), request.period())
            else {
                return;
            };
            // A fixed commitment no harness knows the discrete log of.
            let Ok(stored) = Commitment::decode(&key.public_key().to_bytes()[32..64]) else {
                return;
            };
            assert!(key.issue(&request, &stored).is_err());
        }
        if let Ok(response) = IssuanceResponse::decode(data) {
            assert_eq!(response.to_bytes(), data);
        }
        if let Ok(registration) = CommitmentRegistration::decode(data) {
            assert_eq!(registration.to_bytes(), data);
            assert!(crate::verify_registration(&registration).is_err());
        }
    }

    /// Verify arbitrary bytes as a presentation under a fixed key, scope,
    /// message and period. Nothing the fuzzer produces may verify; anything
    /// that decodes must re-encode to the identical bytes.
    pub fn presentation_verify(data: &[u8]) {
        if let Ok(presentation) = Presentation::decode(data) {
            assert_eq!(presentation.to_bytes(), data);
        }
        let verifier = CredentialVerifier::new(MasterSecret::from_bytes(FUZZ_MASTER));
        let scope = PollScope::channel_post([3u8; 16], 42);
        for period in [FUZZ_PERIOD, FUZZ_PERIOD + 1] {
            assert!(verifier.verify(data, &scope, b"fuzz", period).is_err());
        }
    }
}
