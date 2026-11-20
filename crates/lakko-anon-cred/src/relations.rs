// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The three relations the credential proves, and the transcript each proof
//! is bound to. Prover and verifier build both from these functions, so the
//! two sides cannot describe a relation differently.
//!
//! Notation follows CMZ14 (MAC_GGM with two scalar attributes): generators
//! `G` and `H`; issuer key `(x0, x0t, x1, x2)` with public `C_x0 = x0*G +
//! x0t*H`, `X1 = x1*H`, `X2 = x2*H`; attributes `s` (hidden, the account's
//! credential secret) and `p` (public, the period); a MAC is a pair
//! `(U, U')` with `U' = (x0 + x1*s + x2*p) * U`.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::group::{generator_g, generator_h, hash_to_group};
use crate::key::IssuerPublicKey;
use crate::proof::{Equation, Statement};
use crate::transcript::Transcript;

/// Protocol label of the commitment-registration proof.
const REGISTRATION_PROTOCOL: &[u8] = b"lakko/anon-cred/v1 commitment-registration";
/// Protocol label of the issuance-request proof.
const ISSUANCE_REQUEST_PROTOCOL: &[u8] = b"lakko/anon-cred/v1 issuance-request";
/// Protocol label of the issuance-response proof.
const ISSUANCE_RESPONSE_PROTOCOL: &[u8] = b"lakko/anon-cred/v1 issuance-response";
/// Protocol label of the presentation proof.
const PRESENTATION_PROTOCOL: &[u8] = b"lakko/anon-cred/v1 presentation";

/// The ADR-0092 4.3 tag-base domain, hashed in front of the poll identifier.
pub(crate) const CHANNEL_POLL_TAG_BASE_DOMAIN: &[u8] = b"lakko/channel-poll/tag-base/v1";

/// `H_poll = hash-to-group("lakko/channel-poll/tag-base/v1" || channel_id ||
/// post_cursor_be)` (ADR-0092 4.3), with this crate's hash-to-group label
/// `tag base` in front.
pub(crate) fn channel_poll_tag_base(scope: &[u8]) -> RistrettoPoint {
    let mut input = Vec::with_capacity(CHANNEL_POLL_TAG_BASE_DOMAIN.len() + scope.len());
    input.extend_from_slice(CHANNEL_POLL_TAG_BASE_DOMAIN);
    input.extend_from_slice(scope);
    hash_to_group(b"tag base", &input)
}

/// Knowledge of `s` with `C = s*G`. Witness: `[s]`.
pub(crate) fn commitment_statement(commitment: RistrettoPoint) -> Statement {
    Statement {
        witness_count: 1,
        equations: vec![Equation {
            lhs: commitment,
            terms: vec![(0, generator_g())],
        }],
    }
}

/// Transcript of the proof carried by a commitment registration.
pub(crate) fn registration_transcript(commitment: &RistrettoPoint) -> Transcript {
    let mut t = Transcript::new(REGISTRATION_PROTOCOL);
    t.append_point(b"commitment", commitment);
    t
}

/// Transcript of the proof carried by an issuance request: the period's
/// issuer key and the commitment the request is for.
pub(crate) fn issuance_request_transcript(
    key: &IssuerPublicKey,
    commitment: &RistrettoPoint,
) -> Transcript {
    let mut t = Transcript::new(ISSUANCE_REQUEST_PROTOCOL);
    key.bind(&mut t);
    t.append_point(b"commitment", commitment);
    t
}

/// The issuer's proof that `U'` is a MAC under the published key:
/// `C_x0 = x0*G + x0t*H`, `X1 = x1*H`, `X2 = x2*H`,
/// `U' = x0*U + x1*S + x2*P` with `S = s*U` and `P = p*U`.
/// Witness: `[x0, x0t, x1, x2]`.
pub(crate) fn issuance_statement(
    key: &IssuerPublicKey,
    u: RistrettoPoint,
    s_u: RistrettoPoint,
    u_prime: RistrettoPoint,
) -> Statement {
    let g = generator_g();
    let h = generator_h();
    let p_u = Scalar::from(key.period()) * u;
    Statement {
        witness_count: 4,
        equations: vec![
            Equation {
                lhs: key.c_x0,
                terms: vec![(0, g), (1, h)],
            },
            Equation {
                lhs: key.x1,
                terms: vec![(2, h)],
            },
            Equation {
                lhs: key.x2,
                terms: vec![(3, h)],
            },
            Equation {
                lhs: u_prime,
                terms: vec![(0, u), (2, s_u), (3, p_u)],
            },
        ],
    }
}

/// Transcript of the issuer's proof.
pub(crate) fn issuance_response_transcript(
    key: &IssuerPublicKey,
    commitment: &RistrettoPoint,
    u: &RistrettoPoint,
    u_prime: &RistrettoPoint,
) -> Transcript {
    let mut t = Transcript::new(ISSUANCE_RESPONSE_PROTOCOL);
    key.bind(&mut t);
    t.append_point(b"commitment", commitment);
    t.append_point(b"u", u);
    t.append_point(b"u-prime", u_prime);
    t
}

/// The public points of a presentation.
pub(crate) struct PresentationPoints {
    /// `U~ = r*U`.
    pub(crate) u: RistrettoPoint,
    /// `C_s = s*U~ + z1*H`.
    pub(crate) c_s: RistrettoPoint,
    /// `C_U' = r*U' + z0*G`.
    pub(crate) c_u_prime: RistrettoPoint,
    /// `T = s*H_poll`.
    pub(crate) tag: RistrettoPoint,
}

/// The holder's proof in a presentation:
/// `C_s = s*U~ + z1*H`, `V = z1*X1 + z0n*G`, `T = s*H_poll`, where the
/// verifier computes `V = x0*U~ + x1*C_s + x2*p*U~ - C_U'` from its secret key
/// and the holder computes `V = z1*X1 - z0*G` (equal exactly when the MAC is
/// valid). Witness: `[s, z1, z0n]` with `z0n = -z0`.
pub(crate) fn presentation_statement(
    key: &IssuerPublicKey,
    points: &PresentationPoints,
    v: RistrettoPoint,
    tag_base: RistrettoPoint,
) -> Statement {
    Statement {
        witness_count: 3,
        equations: vec![
            Equation {
                lhs: points.c_s,
                terms: vec![(0, points.u), (1, generator_h())],
            },
            Equation {
                lhs: v,
                terms: vec![(1, key.x1), (2, generator_g())],
            },
            Equation {
                lhs: points.tag,
                terms: vec![(0, tag_base)],
            },
        ],
    }
}

/// Transcript of the presentation proof: the period's issuer key, the poll
/// scope, the caller's bound message and every presented point.
pub(crate) fn presentation_transcript(
    key: &IssuerPublicKey,
    scope: &[u8],
    message: &[u8],
    points: &PresentationPoints,
) -> Transcript {
    let mut t = Transcript::new(PRESENTATION_PROTOCOL);
    key.bind(&mut t);
    t.append(b"scope", scope);
    t.append(b"message", message);
    t.append_point(b"u", &points.u);
    t.append_point(b"c-s", &points.c_s);
    t.append_point(b"c-u-prime", &points.c_u_prime);
    t.append_point(b"tag", &points.tag);
    t
}
