// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The verifier's side (`services/channels`): the poll scope, the presentation
//! wire form, and keyed verification of a presentation.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::client::read_u64;
use crate::error::{AnonCredError, Result};
use crate::group::{decode_nonidentity_point, decode_point};
use crate::key::{period_accepted, IssuerSecretKey, MasterSecret};
use crate::proof::{proof_len, Proof};
use crate::relations::{
    channel_poll_tag_base, presentation_statement, presentation_transcript, PresentationPoints,
};
use crate::{MAX_MESSAGE_LEN, WIRE_VERSION};

/// Length of a channel poll scope: `channel_id(16) || post_cursor(8, BE)`.
pub const SCOPE_LEN: usize = 16 + 8;

/// Length of a presentation:
/// `version || period || U~ || C_s || C_U' || T || proof(3 witnesses)`.
pub const PRESENTATION_LEN: usize = 1 + 8 + 4 * 32 + proof_len(3);

/// Length of a tag.
pub const TAG_LEN: usize = 32;

/// The poll a presentation is for: `(channel_id, post_cursor)`, the pair the
/// reaction route already names (ADR-0090 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PollScope {
    bytes: [u8; SCOPE_LEN],
}

impl PollScope {
    /// The poll posted at `post_cursor` in channel `channel_id`.
    #[must_use]
    pub fn channel_post(channel_id: [u8; 16], post_cursor: u64) -> Self {
        let mut bytes = [0u8; SCOPE_LEN];
        bytes[..16].copy_from_slice(&channel_id);
        bytes[16..].copy_from_slice(&post_cursor.to_be_bytes());
        Self { bytes }
    }

    /// `channel_id || post_cursor_be`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SCOPE_LEN] {
        &self.bytes
    }

    /// `H_poll`, the base of this poll's tag.
    pub(crate) fn tag_base(&self) -> RistrettoPoint {
        channel_poll_tag_base(&self.bytes)
    }
}

/// A presentation of a credential for one poll.
///
/// Wire: `version(1) || period(8, BE) || U~(32) || C_s(32) || C_U'(32) ||
/// T(32) || challenge(32) || 3 responses(96)` (265 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    period: u64,
    u: RistrettoPoint,
    c_s: RistrettoPoint,
    c_u_prime: RistrettoPoint,
    tag: RistrettoPoint,
    proof: Proof,
}

impl Presentation {
    pub(crate) fn from_parts(period: u64, points: PresentationPoints, proof: Proof) -> Self {
        Self {
            period,
            u: points.u,
            c_s: points.c_s,
            c_u_prime: points.c_u_prime,
            tag: points.tag,
            proof,
        }
    }

    /// The period of the credential presented.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// The tag `T = s*H_poll`, as it appears in the vote route's path.
    /// Unverified until [`CredentialVerifier::verify`] accepts the
    /// presentation.
    #[must_use]
    pub fn tag(&self) -> [u8; TAG_LEN] {
        self.tag.compress().to_bytes()
    }

    /// The wire form.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PRESENTATION_LEN);
        out.push(WIRE_VERSION);
        out.extend_from_slice(&self.period.to_be_bytes());
        for p in [&self.u, &self.c_s, &self.c_u_prime, &self.tag] {
            out.extend_from_slice(p.compress().as_bytes());
        }
        out.extend_from_slice(&self.proof.to_bytes());
        out
    }

    /// Structural decode: exact length, version, canonical points (`U~` and
    /// `T` not the identity), canonical proof scalars. Does not verify.
    ///
    /// # Errors
    ///
    /// `Malformed`, `UnknownVersion`, `NonCanonical`, `IdentityPoint`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != PRESENTATION_LEN {
            return Err(AnonCredError::Malformed);
        }
        if bytes[0] != WIRE_VERSION {
            return Err(AnonCredError::UnknownVersion);
        }
        Ok(Self {
            period: read_u64(&bytes[1..9])?,
            u: decode_nonidentity_point(&bytes[9..41])?,
            c_s: decode_point(&bytes[41..73])?,
            c_u_prime: decode_point(&bytes[73..105])?,
            tag: decode_nonidentity_point(&bytes[105..137])?,
            proof: Proof::decode(&bytes[137..], 3)?,
        })
    }

    fn points(&self) -> PresentationPoints {
        PresentationPoints {
            u: self.u,
            c_s: self.c_s,
            c_u_prime: self.c_u_prime,
            tag: self.tag,
        }
    }
}

/// What a verified presentation establishes: some account holding a valid
/// credential for `period` produced `tag` for this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedPresentation {
    /// The tag, the same for one account and one poll in every period.
    pub tag: [u8; TAG_LEN],
    /// The period of the credential presented.
    pub period: u64,
}

impl IssuerSecretKey {
    /// Verify `presentation` under this period's key, for `scope` and bound
    /// `message`.
    ///
    /// Computes `V = x0*U~ + x1*C_s + x2*p*U~ - C_U'` and checks the holder's
    /// proof that `C_s` commits to the `s` of the tag and that `V` opens as a
    /// valid MAC would. The period window is the caller's
    /// ([`CredentialVerifier::verify`] checks it).
    ///
    /// # Errors
    ///
    /// `WrongPeriod`, `MessageTooLong`, `BadProof`.
    pub fn verify_presentation(
        &self,
        presentation: &Presentation,
        scope: &PollScope,
        message: &[u8],
    ) -> Result<VerifiedPresentation> {
        if presentation.period != self.period() {
            return Err(AnonCredError::WrongPeriod);
        }
        if message.len() > MAX_MESSAGE_LEN {
            return Err(AnonCredError::MessageTooLong);
        }
        let public = self.public_key();
        let points = presentation.points();
        let p = Scalar::from(self.period());
        let v =
            self.x0 * points.u + self.x1 * points.c_s + (self.x2 * p) * points.u - points.c_u_prime;
        let statement = presentation_statement(&public, &points, v, scope.tag_base());
        let transcript = presentation_transcript(&public, scope.as_bytes(), message, &points);
        if !presentation.proof.verify(transcript, &statement) {
            return Err(AnonCredError::BadProof);
        }
        Ok(VerifiedPresentation {
            tag: presentation.tag(),
            period: presentation.period,
        })
    }
}

/// The verifying service's handle: the shared master secret.
#[derive(Debug, Clone)]
pub struct CredentialVerifier {
    master: MasterSecret,
}

impl CredentialVerifier {
    /// A verifier over the master secret shared with the issuer.
    #[must_use]
    pub const fn new(master: MasterSecret) -> Self {
        Self { master }
    }

    /// Decode and verify a presentation for `scope` and `message` at
    /// `current_period`: structural decode, the period window (its own period
    /// and the next), then [`IssuerSecretKey::verify_presentation`] under the
    /// key derived for the presented period.
    ///
    /// The caller compares the returned tag with the one its route names and
    /// enforces one row per (poll, tag).
    ///
    /// # Errors
    ///
    /// Any decoding error, `PeriodNotAccepted`, `MessageTooLong`, `BadProof`.
    pub fn verify(
        &self,
        presentation: &[u8],
        scope: &PollScope,
        message: &[u8],
        current_period: u64,
    ) -> Result<VerifiedPresentation> {
        if message.len() > MAX_MESSAGE_LEN {
            return Err(AnonCredError::MessageTooLong);
        }
        let presentation = Presentation::decode(presentation)?;
        if !period_accepted(presentation.period, current_period) {
            return Err(AnonCredError::PeriodNotAccepted);
        }
        let key = IssuerSecretKey::derive(&self.master, presentation.period)?;
        key.verify_presentation(&presentation, scope, message)
    }
}
