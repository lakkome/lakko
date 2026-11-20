// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A non-interactive proof of knowledge of scalars satisfying a set of linear
//! relations between Ristretto255 points.
//!
//! A statement is a list of equations `P_j = sum_k w_{i(j,k)} * B_{j,k}`
//! over `n` secret witnesses `w_0 .. w_{n-1}`; the points `P_j` and `B_{j,k}`
//! are public. This is the generalised Schnorr protocol (Camenisch-Stadler
//! notation, the shape CMZ14 and Signal's `poksho` prove every credential
//! relation in), made non-interactive with Fiat-Shamir:
//!
//! - prover: nonces `k_i`, commitments `A_j = sum k_{i(j,k)} * B_{j,k}`,
//!   challenge `c = H(transcript, statement, A_0 .. A_m)`, responses
//!   `r_i = k_i - c * w_i`;
//! - verifier: `A_j' = sum r_{i(j,k)} * B_{j,k} + c * P_j`, accept iff
//!   `H(transcript, statement, A_0' .. A_m') == c`.
//!
//! The proof is the compact form `c || r_0 || .. || r_{n-1}`
//! (`32 * (n + 1)` bytes), as in `poksho`. The statement itself (every base,
//! every left-hand side, the witness index of every term) is appended to the
//! transcript before the commitments, so a proof cannot be moved to another
//! statement with the same shape.
//!
//! Nonces are hedged: each is derived from fresh OS randomness together with
//! the transcript state and the witness, so a weak RNG alone does not repeat a
//! nonce across two different statements or witnesses.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::{Identity, MultiscalarMul, VartimeMultiscalarMul};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{AnonCredError, Result};
use crate::group::{decode_scalar, random_bytes};
use crate::transcript::Transcript;

/// BLAKE3 derive-key context of the hedged nonce derivation.
const NONCE_CONTEXT: &str = "lakko/anon-cred/v1 proof-nonce";

/// One equation `lhs = sum_k witness[index_k] * base_k`.
pub(crate) struct Equation {
    /// The public left-hand side.
    pub(crate) lhs: RistrettoPoint,
    /// `(witness index, public base)` terms.
    pub(crate) terms: Vec<(usize, RistrettoPoint)>,
}

/// A statement: the number of witnesses and the equations they satisfy.
pub(crate) struct Statement {
    /// Number of secret scalars.
    pub(crate) witness_count: usize,
    /// The relations.
    pub(crate) equations: Vec<Equation>,
}

impl Statement {
    fn bind(&self, transcript: &mut Transcript) {
        transcript.append_u64(b"witness-count", self.witness_count as u64);
        transcript.append_u64(b"equation-count", self.equations.len() as u64);
        for equation in &self.equations {
            transcript.append_point(b"lhs", &equation.lhs);
            transcript.append_u64(b"term-count", equation.terms.len() as u64);
            for (index, base) in &equation.terms {
                transcript.append_u64(b"witness-index", *index as u64);
                transcript.append_point(b"base", base);
            }
        }
    }

    /// Every term index must name a witness.
    fn indices_valid(&self) -> bool {
        self.equations
            .iter()
            .all(|e| e.terms.iter().all(|(i, _)| *i < self.witness_count))
    }

    /// `sum_k scalars[index_k] * base_k` for one equation, constant time.
    fn evaluate(equation: &Equation, scalars: &[Scalar]) -> Option<RistrettoPoint> {
        let mut picked = Vec::with_capacity(equation.terms.len());
        for (index, _) in &equation.terms {
            picked.push(*scalars.get(*index)?);
        }
        if picked.is_empty() {
            return Some(RistrettoPoint::identity());
        }
        Some(RistrettoPoint::multiscalar_mul(
            picked.iter(),
            equation.terms.iter().map(|(_, b)| b),
        ))
    }
}

/// A compact proof: the challenge and one response per witness.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Proof {
    challenge: Scalar,
    responses: Vec<Scalar>,
}

impl core::fmt::Debug for Proof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Proof")
            .field("responses", &self.responses.len())
            .finish_non_exhaustive()
    }
}

/// Length in bytes of a proof over `witness_count` witnesses.
pub(crate) const fn proof_len(witness_count: usize) -> usize {
    32 * (witness_count + 1)
}

impl Proof {
    /// Prove knowledge of `witness` for `statement` under `transcript`.
    ///
    /// Refuses (`Internal`) a witness of the wrong length or one that does not
    /// satisfy every equation: a proof is never produced for a false statement.
    pub(crate) fn prove(
        mut transcript: Transcript,
        statement: &Statement,
        witness: &[Scalar],
    ) -> Result<Self> {
        if witness.len() != statement.witness_count || !statement.indices_valid() {
            return Err(AnonCredError::Internal);
        }
        for equation in &statement.equations {
            if Statement::evaluate(equation, witness) != Some(equation.lhs) {
                return Err(AnonCredError::Internal);
            }
        }
        statement.bind(&mut transcript);

        let state = transcript.state_digest();
        let entropy = random_bytes()?;
        let mut nonce_values: Zeroizing<Vec<Scalar>> =
            Zeroizing::new(Vec::with_capacity(witness.len()));
        for (i, w) in witness.iter().enumerate() {
            let mut hasher = blake3::Hasher::new_derive_key(NONCE_CONTEXT);
            hasher.update(&state);
            hasher.update(entropy.as_ref());
            hasher.update(&(i as u64).to_be_bytes());
            hasher.update(w.as_bytes());
            let mut wide = Zeroizing::new([0u8; 64]);
            hasher.finalize_xof().fill(wide.as_mut());
            nonce_values.push(Scalar::from_bytes_mod_order_wide(&wide));
        }

        for equation in &statement.equations {
            let commitment =
                Statement::evaluate(equation, &nonce_values).ok_or(AnonCredError::Internal)?;
            transcript.append_point(b"commitment", &commitment);
        }
        let challenge = transcript.challenge();
        let responses = nonce_values
            .iter()
            .zip(witness)
            .map(|(k, w)| k - challenge * w)
            .collect();
        Ok(Self {
            challenge,
            responses,
        })
    }

    /// Verify this proof for `statement` under `transcript`.
    pub(crate) fn verify(&self, mut transcript: Transcript, statement: &Statement) -> bool {
        if self.responses.len() != statement.witness_count || !statement.indices_valid() {
            return false;
        }
        statement.bind(&mut transcript);
        for equation in &statement.equations {
            let mut scalars = Vec::with_capacity(equation.terms.len() + 1);
            let mut points = Vec::with_capacity(equation.terms.len() + 1);
            for (index, base) in &equation.terms {
                let Some(r) = self.responses.get(*index) else {
                    return false;
                };
                scalars.push(*r);
                points.push(*base);
            }
            scalars.push(self.challenge);
            points.push(equation.lhs);
            let commitment = RistrettoPoint::vartime_multiscalar_mul(scalars.iter(), points.iter());
            transcript.append_point(b"commitment", &commitment);
        }
        bool::from(transcript.challenge().ct_eq(&self.challenge))
    }

    /// `challenge || responses`, each a 32-byte little-endian scalar.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(proof_len(self.responses.len()));
        out.extend_from_slice(self.challenge.as_bytes());
        for r in &self.responses {
            out.extend_from_slice(r.as_bytes());
        }
        out
    }

    /// Decode a proof over exactly `witness_count` witnesses. Every scalar
    /// must be canonical.
    pub(crate) fn decode(bytes: &[u8], witness_count: usize) -> Result<Self> {
        if bytes.len() != proof_len(witness_count) {
            return Err(AnonCredError::Malformed);
        }
        let mut chunks = bytes.chunks_exact(32);
        let challenge = decode_scalar(chunks.next().ok_or(AnonCredError::Malformed)?)?;
        let responses = chunks.map(decode_scalar).collect::<Result<Vec<_>>>()?;
        Ok(Self {
            challenge,
            responses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{generator_g, generator_h, random_nonzero_scalar};

    fn dleq(x: Scalar, h2: RistrettoPoint) -> Statement {
        Statement {
            witness_count: 1,
            equations: vec![
                Equation {
                    lhs: x * generator_g(),
                    terms: vec![(0, generator_g())],
                },
                Equation {
                    lhs: x * h2,
                    terms: vec![(0, h2)],
                },
            ],
        }
    }

    #[test]
    fn completeness_and_soundness_checks() {
        let x = random_nonzero_scalar().unwrap();
        let h2 = generator_h();
        let stmt = dleq(x, h2);
        let proof = Proof::prove(Transcript::new(b"t"), &stmt, &[x]).unwrap();
        assert!(proof.verify(Transcript::new(b"t"), &stmt));
        // Other transcript.
        assert!(!proof.verify(Transcript::new(b"u"), &stmt));
        // Other statement.
        let y = random_nonzero_scalar().unwrap();
        assert!(!proof.verify(Transcript::new(b"t"), &dleq(y, h2)));
        // Round trip.
        let decoded = Proof::decode(&proof.to_bytes(), 1).unwrap();
        assert_eq!(decoded, proof);
        assert!(Proof::decode(&proof.to_bytes(), 2).is_err());
    }

    #[test]
    fn false_statement_is_refused_by_prover() {
        let x = random_nonzero_scalar().unwrap();
        let y = random_nonzero_scalar().unwrap();
        let mut stmt = dleq(x, generator_h());
        stmt.equations[1].lhs = y * generator_h();
        assert_eq!(
            Proof::prove(Transcript::new(b"t"), &stmt, &[x]).unwrap_err(),
            AnonCredError::Internal
        );
    }

    #[test]
    fn bad_index_is_refused() {
        let x = random_nonzero_scalar().unwrap();
        let mut stmt = dleq(x, generator_h());
        let proof = Proof::prove(Transcript::new(b"t"), &stmt, &[x]).unwrap();
        stmt.equations[0].terms[0].0 = 5;
        assert!(!proof.verify(Transcript::new(b"t"), &stmt));
    }
}
