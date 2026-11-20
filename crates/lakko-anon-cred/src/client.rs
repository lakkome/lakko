// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The holder's side: the credential secret, the commitment registration,
//! the issuance request, accepting an issuance response, the stored
//! credential, and building a presentation.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{AnonCredError, Result};
use crate::group::{decode_nonidentity_point, generator_g, generator_h, random_nonzero_scalar};
use crate::issuer::IssuanceResponse;
use crate::key::IssuerPublicKey;
use crate::proof::{proof_len, Proof};
use crate::relations::{
    commitment_statement, issuance_request_transcript, issuance_response_transcript,
    issuance_statement, presentation_statement, presentation_transcript, registration_transcript,
    PresentationPoints,
};
use crate::verifier::{PollScope, Presentation};
use crate::{MAX_MESSAGE_LEN, WIRE_VERSION};

/// BLAKE3 derive-key context of the credential secret (ADR-0092 4.2).
pub const SECRET_DERIVE_CONTEXT: &str = "lakko/anon-credential/secret/v1";

/// Length of an encoded commitment.
pub const COMMITMENT_LEN: usize = 32;
/// Length of a commitment registration: `version || C || proof(1 witness)`.
pub const REGISTRATION_LEN: usize = 1 + COMMITMENT_LEN + proof_len(1);
/// Length of an issuance request: `version || period || proof(1 witness)`.
pub const ISSUANCE_REQUEST_LEN: usize = 1 + 8 + proof_len(1);
/// Length of a stored credential: `version || period || U || U'`.
pub const CREDENTIAL_LEN: usize = 1 + 8 + 32 + 32;

/// The account's credential secret `s`.
///
/// Derived from the account's lineage seed, so every device of the account
/// computes the same `s` and nothing is synced. Wiped on drop; `Debug` prints
/// nothing of the value.
pub struct CredentialSecret {
    s: Scalar,
}

impl Drop for CredentialSecret {
    fn drop(&mut self) {
        self.s.zeroize();
    }
}

impl core::fmt::Debug for CredentialSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CredentialSecret(..)")
    }
}

impl CredentialSecret {
    /// `s` from the 32-byte lineage seed (ADR-0078 phase 6, the oldest own
    /// account seed).
    ///
    /// `BLAKE3-derive-key("lakko/anon-credential/secret/v1", lineage_seed)` in
    /// XOF mode, 64 bytes, reduced modulo the group order. The first 32 bytes
    /// of that output are the ADR's `BLAKE3-derive-key` value; the extension to
    /// 64 bytes is what makes the reduction unbiased.
    ///
    /// # Errors
    ///
    /// [`AnonCredError::ZeroScalar`] if the reduction is zero (probability
    /// about 2^-252).
    pub fn from_lineage_seed(lineage_seed: &[u8; 32]) -> Result<Self> {
        let mut hasher = blake3::Hasher::new_derive_key(SECRET_DERIVE_CONTEXT);
        hasher.update(lineage_seed);
        let mut wide = Zeroizing::new([0u8; 64]);
        hasher.finalize_xof().fill(wide.as_mut());
        let s = Scalar::from_bytes_mod_order_wide(&wide);
        if s == Scalar::ZERO {
            return Err(AnonCredError::ZeroScalar);
        }
        Ok(Self { s })
    }

    /// The commitment `C = s*G` auth stores for the account.
    #[must_use]
    pub fn commitment(&self) -> Commitment {
        Commitment(self.s * generator_g())
    }

    /// A commitment registration for `PUT /auth/anon-credential/commitment`:
    /// `C` with a proof of knowledge of `s`.
    ///
    /// # Errors
    ///
    /// `RandomnessUnavailable` if the OS RNG fails.
    pub fn register(&self) -> Result<CommitmentRegistration> {
        let commitment = self.commitment();
        let proof = Proof::prove(
            registration_transcript(&commitment.0),
            &commitment_statement(commitment.0),
            &[self.s],
        )?;
        Ok(CommitmentRegistration { commitment, proof })
    }

    /// An issuance request for `key`'s period, proving knowledge of the `s`
    /// behind this account's commitment.
    ///
    /// `key` must be the public key fetched from the issuer's key route for
    /// the period, never one taken from a response.
    ///
    /// # Errors
    ///
    /// `RandomnessUnavailable` if the OS RNG fails.
    pub fn issuance_request(&self, key: &IssuerPublicKey) -> Result<IssuanceRequest> {
        let commitment = self.commitment();
        let proof = Proof::prove(
            issuance_request_transcript(key, &commitment.0),
            &commitment_statement(commitment.0),
            &[self.s],
        )?;
        Ok(IssuanceRequest {
            period: key.period(),
            proof,
        })
    }

    /// Accept an issuance response: check it names `key`'s period, that `U` is
    /// not the identity and that the issuer's proof shows `U'` was computed
    /// under `key` for this account's `s` and the period. Returns the
    /// credential to store.
    ///
    /// # Errors
    ///
    /// `WrongPeriod`, `BadProof`, or a decoding error.
    pub fn finalize_issuance(
        &self,
        key: &IssuerPublicKey,
        response: &IssuanceResponse,
    ) -> Result<Credential> {
        if response.period != key.period() {
            return Err(AnonCredError::WrongPeriod);
        }
        let commitment = self.commitment();
        let s_u = self.s * response.u;
        let statement = issuance_statement(key, response.u, s_u, response.u_prime);
        let transcript =
            issuance_response_transcript(key, &commitment.0, &response.u, &response.u_prime);
        if !response.proof.verify(transcript, &statement) {
            return Err(AnonCredError::BadProof);
        }
        Ok(Credential {
            period: response.period,
            u: response.u,
            u_prime: response.u_prime,
        })
    }

    /// A fresh presentation of `credential` for the poll `scope`, bound to
    /// `message` (the caller's request content: the vote's labels, or a
    /// retract).
    ///
    /// Every call draws fresh `r`, `z0`, `z1` and proof nonces, so two
    /// presentations of one credential share no value except the tag, and
    /// share the tag only for the same scope.
    ///
    /// # Errors
    ///
    /// `WrongPeriod` if `key` is not the credential's period,
    /// `MessageTooLong`, `RandomnessUnavailable`.
    // The names follow CMZ14's notation so the code reads against the paper.
    #[allow(clippy::many_single_char_names)]
    pub fn present(
        &self,
        credential: &Credential,
        key: &IssuerPublicKey,
        scope: &PollScope,
        message: &[u8],
    ) -> Result<Presentation> {
        if credential.period != key.period() {
            return Err(AnonCredError::WrongPeriod);
        }
        if message.len() > MAX_MESSAGE_LEN {
            return Err(AnonCredError::MessageTooLong);
        }
        let r = Zeroizing::new(random_nonzero_scalar()?);
        let z0 = Zeroizing::new(random_nonzero_scalar()?);
        let z1 = Zeroizing::new(random_nonzero_scalar()?);
        let g = generator_g();
        let h = generator_h();

        let tag_base = scope.tag_base();
        let u = *r * credential.u;
        let points = PresentationPoints {
            u,
            c_s: self.s * u + *z1 * h,
            c_u_prime: *r * credential.u_prime + *z0 * g,
            tag: self.s * tag_base,
        };
        let v = *z1 * key.x1 - *z0 * g;
        let statement = presentation_statement(key, &points, v, tag_base);
        let transcript = presentation_transcript(key, scope.as_bytes(), message, &points);
        let z0n = Zeroizing::new(-*z0);
        let proof = Proof::prove(transcript, &statement, &[self.s, *z1, *z0n])?;
        Ok(Presentation::from_parts(credential.period, points, proof))
    }
}

/// The account commitment `C = s*G` (never the identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commitment(pub(crate) RistrettoPoint);

impl Commitment {
    /// The 32-byte compressed point auth stores.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; COMMITMENT_LEN] {
        self.0.compress().to_bytes()
    }

    /// Decode a stored commitment.
    ///
    /// # Errors
    ///
    /// `Malformed`, `NonCanonical` or `IdentityPoint`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self(decode_nonidentity_point(bytes)?))
    }
}

/// A commitment with a proof that the sender knows its discrete log.
///
/// Wire: `version(1) || C(32) || challenge(32) || response(32)` (97 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitmentRegistration {
    pub(crate) commitment: Commitment,
    pub(crate) proof: Proof,
}

impl CommitmentRegistration {
    /// The commitment being registered (unverified until
    /// [`crate::verify_registration`] accepts it).
    #[must_use]
    pub const fn commitment(&self) -> Commitment {
        self.commitment
    }

    /// The wire form.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(REGISTRATION_LEN);
        out.push(WIRE_VERSION);
        out.extend_from_slice(&self.commitment.to_bytes());
        out.extend_from_slice(&self.proof.to_bytes());
        out
    }

    /// Structural decode: exact length, version, canonical non-identity `C`,
    /// canonical proof scalars. Does not verify the proof.
    ///
    /// # Errors
    ///
    /// `Malformed`, `UnknownVersion`, `NonCanonical`, `IdentityPoint`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != REGISTRATION_LEN {
            return Err(AnonCredError::Malformed);
        }
        if bytes[0] != WIRE_VERSION {
            return Err(AnonCredError::UnknownVersion);
        }
        Ok(Self {
            commitment: Commitment::decode(&bytes[1..33])?,
            proof: Proof::decode(&bytes[33..], 1)?,
        })
    }
}

/// A request for a credential for one period.
///
/// Wire: `version(1) || period(8, BE) || challenge(32) || response(32)`
/// (73 bytes). The commitment is not repeated: the issuer verifies the proof
/// against the commitment it stores for the session's account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuanceRequest {
    pub(crate) period: u64,
    pub(crate) proof: Proof,
}

impl IssuanceRequest {
    /// The period the request is for.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// The wire form.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ISSUANCE_REQUEST_LEN);
        out.push(WIRE_VERSION);
        out.extend_from_slice(&self.period.to_be_bytes());
        out.extend_from_slice(&self.proof.to_bytes());
        out
    }

    /// Structural decode: exact length, version, canonical proof scalars.
    /// Does not verify the proof.
    ///
    /// # Errors
    ///
    /// `Malformed`, `UnknownVersion`, `NonCanonical`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != ISSUANCE_REQUEST_LEN {
            return Err(AnonCredError::Malformed);
        }
        if bytes[0] != WIRE_VERSION {
            return Err(AnonCredError::UnknownVersion);
        }
        Ok(Self {
            period: read_u64(&bytes[1..9])?,
            proof: Proof::decode(&bytes[9..], 1)?,
        })
    }
}

/// A credential: a MAC `(U, U')` on `(s, period)`.
///
/// Wire (client storage only, never sent): `version(1) || period(8, BE) ||
/// U(32) || U'(32)` (73 bytes). Holding it without `s` is useless: every
/// presentation proves knowledge of `s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credential {
    pub(crate) period: u64,
    pub(crate) u: RistrettoPoint,
    pub(crate) u_prime: RistrettoPoint,
}

impl Credential {
    /// The period the credential is for.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// The storage form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CREDENTIAL_LEN] {
        let mut out = [0u8; CREDENTIAL_LEN];
        out[0] = WIRE_VERSION;
        out[1..9].copy_from_slice(&self.period.to_be_bytes());
        out[9..41].copy_from_slice(self.u.compress().as_bytes());
        out[41..].copy_from_slice(self.u_prime.compress().as_bytes());
        out
    }

    /// Decode a stored credential: exact length, version, canonical
    /// non-identity `U` and `U'`.
    ///
    /// # Errors
    ///
    /// `Malformed`, `UnknownVersion`, `NonCanonical`, `IdentityPoint`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != CREDENTIAL_LEN {
            return Err(AnonCredError::Malformed);
        }
        if bytes[0] != WIRE_VERSION {
            return Err(AnonCredError::UnknownVersion);
        }
        Ok(Self {
            period: read_u64(&bytes[1..9])?,
            u: decode_nonidentity_point(&bytes[9..41])?,
            u_prime: decode_nonidentity_point(&bytes[41..])?,
        })
    }
}

pub(crate) fn read_u64(bytes: &[u8]) -> Result<u64> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| AnonCredError::Malformed)?;
    Ok(u64::from_be_bytes(array))
}
