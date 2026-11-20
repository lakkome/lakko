// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The issuer's side (`services/auth`): accept a commitment registration and
//! answer an issuance request with a MAC and a proof that it was computed
//! under the period's published key.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use zeroize::Zeroizing;

use crate::client::{read_u64, Commitment, CommitmentRegistration, IssuanceRequest};
use crate::error::{AnonCredError, Result};
use crate::group::{decode_nonidentity_point, generator_g, random_nonzero_scalar};
use crate::key::IssuerSecretKey;
use crate::proof::{proof_len, Proof};
use crate::relations::{
    commitment_statement, issuance_request_transcript, issuance_response_transcript,
    issuance_statement, registration_transcript,
};
use crate::WIRE_VERSION;

/// Length of an issuance response:
/// `version || period || U || U' || proof(4 witnesses)`.
pub const ISSUANCE_RESPONSE_LEN: usize = 1 + 8 + 32 + 32 + proof_len(4);

/// Verify a commitment registration and return the commitment to store.
///
/// # Errors
///
/// `BadProof` if the sender did not prove knowledge of the commitment's
/// discrete log.
pub fn verify_registration(registration: &CommitmentRegistration) -> Result<Commitment> {
    let c = registration.commitment.0;
    if registration
        .proof
        .verify(registration_transcript(&c), &commitment_statement(c))
    {
        Ok(registration.commitment)
    } else {
        Err(AnonCredError::BadProof)
    }
}

impl IssuerSecretKey {
    /// Answer an issuance request for the account whose stored commitment is
    /// `stored`.
    ///
    /// Checks the request is for this key's period and that its proof shows
    /// knowledge of `s` with `stored = s*G`, bound to this period's public
    /// key. Then draws a fresh non-zero `b`, sets `U = b*G`, `S = b*C` (equal
    /// to `s*U`, computed without `s`), `U' = x0*U + x1*S + x2*p*U`, and proves
    /// that `U'` used the published key.
    ///
    /// The issuer never learns `s`. The quota (one per account per period) is
    /// the caller's.
    ///
    /// # Errors
    ///
    /// `WrongPeriod`, `BadProof`, `RandomnessUnavailable`.
    pub fn issue(
        &self,
        request: &IssuanceRequest,
        stored: &Commitment,
    ) -> Result<IssuanceResponse> {
        if request.period != self.period() {
            return Err(AnonCredError::WrongPeriod);
        }
        let public = self.public_key();
        let c = stored.0;
        if !request.proof.verify(
            issuance_request_transcript(&public, &c),
            &commitment_statement(c),
        ) {
            return Err(AnonCredError::BadProof);
        }
        let b = Zeroizing::new(random_nonzero_scalar()?);
        let u = *b * generator_g();
        let s_u = *b * c;
        let period = Scalar::from(self.period());
        let u_prime = self.x0 * u + self.x1 * s_u + (self.x2 * period) * u;
        let statement = issuance_statement(&public, u, s_u, u_prime);
        let transcript = issuance_response_transcript(&public, &c, &u, &u_prime);
        let proof = Proof::prove(
            transcript,
            &statement,
            &[self.x0, self.x0t, self.x1, self.x2],
        )?;
        Ok(IssuanceResponse {
            period: self.period(),
            u,
            u_prime,
            proof,
        })
    }
}

/// The issuer's answer: the MAC `(U, U')` and its proof.
///
/// Wire: `version(1) || period(8, BE) || U(32) || U'(32) || challenge(32) ||
/// 4 responses(128)` (233 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuanceResponse {
    pub(crate) period: u64,
    pub(crate) u: RistrettoPoint,
    pub(crate) u_prime: RistrettoPoint,
    pub(crate) proof: Proof,
}

impl IssuanceResponse {
    /// The period the response is for.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// The wire form.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ISSUANCE_RESPONSE_LEN);
        out.push(WIRE_VERSION);
        out.extend_from_slice(&self.period.to_be_bytes());
        out.extend_from_slice(self.u.compress().as_bytes());
        out.extend_from_slice(self.u_prime.compress().as_bytes());
        out.extend_from_slice(&self.proof.to_bytes());
        out
    }

    /// Structural decode: exact length, version, canonical non-identity `U`
    /// and `U'`, canonical proof scalars. Does not verify the proof.
    ///
    /// # Errors
    ///
    /// `Malformed`, `UnknownVersion`, `NonCanonical`, `IdentityPoint`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != ISSUANCE_RESPONSE_LEN {
            return Err(AnonCredError::Malformed);
        }
        if bytes[0] != WIRE_VERSION {
            return Err(AnonCredError::UnknownVersion);
        }
        Ok(Self {
            period: read_u64(&bytes[1..9])?,
            u: decode_nonidentity_point(&bytes[9..41])?,
            u_prime: decode_nonidentity_point(&bytes[41..73])?,
            proof: Proof::decode(&bytes[73..], 4)?,
        })
    }
}
