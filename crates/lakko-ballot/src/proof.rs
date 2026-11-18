// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The linkable one-out-of-many proof (ADR-0090 section 3.6).
//!
//! The construction is the Triptych proof of Noether and Goodell (2020), in
//! its plain linkable form without the commitment-to-zero component, which is
//! the one-out-of-many proof of Groth and Kohlweiss (2015) with the bit
//! commitment of Bootle et al. (2015) and a linking tag. It proves, for a ring
//! `M_0 .. M_{L-1}` of public keys, a tag base `U` and a tag `J`:
//!
//! ```text
//! knowledge of (l, r) with M_l = r G and r J = U
//! ```
//!
//! without revealing `l`. `G` is the Ed25519 base point. Base `n = 2`; the
//! ring is padded to `N = 2^m` (`m >= 1`) by repeating its last key, which
//! adds no key to the set a prover may know a secret of.
//!
//! Prover, with the digits `l_j` of `l` and `sigma_{j,i} = [l_j == i]`:
//!
//! ```text
//! a_{j,i} random for i >= 1, a_{j,0} = -sum_{i>=1} a_{j,i}
//! A = r_A H + sum a_{j,i} G_{j,i}             B = r_B H + sum sigma_{j,i} G_{j,i}
//! C = r_C H + sum a_{j,i}(1 - 2 sigma_{j,i}) G_{j,i}
//! D = r_D H - sum a_{j,i}^2 G_{j,i}
//! p_k(X) = prod_j (sigma_{j,k_j} X + a_{j,k_j}) = [k == l] X^m + sum_{j<m} p_{k,j} X^j
//! X_j = sum_k p_{k,j} M_k + rho_j G          Y_j = rho_j J
//! x = challenge(transcript)
//! f_{j,i} = sigma_{j,i} x + a_{j,i}  (i >= 1, sent)
//! z_A = r_A + x r_B    z_C = x r_C + r_D    z = r x^m - sum_j rho_j x^j
//! ```
//!
//! Verifier, with `f_{j,0} = x - sum_{i>=1} f_{j,i}`:
//!
//! ```text
//! A + x B            == z_A H + sum f_{j,i} G_{j,i}
//! x C + D            == z_C H + sum f_{j,i} (x - f_{j,i}) G_{j,i}
//! sum_k (prod_j f_{j,k_j}) M_k - sum_j x^j X_j == z G
//! x^m U - sum_j x^j Y_j                        == z J
//! ```
//!
//! The challenge is the Fiat-Shamir hash of a transcript that binds the
//! parameters, the ring (by a hash of its keys), the caller's context, `U`,
//! `J` and every commitment, in that order (see `transcript`).
//!
//! Deviations from the papers, each deliberate:
//!
//! - The generators `H`, `G_{j,i}` and the tag base `U` come from
//!   [`crate::point`]'s try-and-increment map, not RFC 9380.
//! - The prover's random scalars are hedged: derived from fresh OS
//!   randomness, the secret and the statement, so a broken random source
//!   alone does not repeat them.
//! - Each verification equation is checked as its own multiscalar
//!   multiplication; no random linear combination and no batching across
//!   proofs (the papers' batch verification is not built).

use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::{IsIdentity, MultiscalarMul, VartimeMultiscalarMul};
use zeroize::Zeroize;

use crate::error::{BallotError, Result};
use crate::point::{commitment_generators, decode_point, decode_scalar, POINT_LEN, SCALAR_LEN};
use crate::transcript::Transcript;

/// The base of the index digits.
pub const BASE: usize = 2;

/// The largest number of digits a proof may have: a proof is over at most
/// `BASE^MAX_DIGITS` = 256 keys, the anonymity set of
/// [`crate::ballot::ANONYMITY_SET`].
pub const MAX_DIGITS: usize = 8;

/// Domain of the proof transcript.
const PROOF_DOMAIN: &str = "lakko/poll/ballot-proof/v1";
/// Domain of the ring digest inside the transcript.
const RING_DIGEST_DOMAIN: &str = "lakko/poll/ballot-proof/ring/v1";
/// Domain of the hedged prover randomness.
const NONCE_DOMAIN: &str = "lakko/poll/ballot-proof/nonce/v1";

/// `BASE^m`, the padded ring size for `m` digits.
#[must_use]
pub const fn capacity(m: usize) -> usize {
    let mut value = 1usize;
    let mut i = 0;
    while i < m {
        value *= BASE;
        i += 1;
    }
    value
}

/// The number of digits for a ring of `len` keys: the smallest `m >= 1` with
/// `BASE^m >= len`.
pub fn digits_for(len: usize) -> Result<usize> {
    if len == 0 {
        return Err(BallotError::InvalidRing);
    }
    let mut m = 1usize;
    let mut capacity = BASE;
    while capacity < len {
        m += 1;
        if m > MAX_DIGITS {
            return Err(BallotError::RingTooLarge);
        }
        capacity *= BASE;
    }
    Ok(m)
}

/// The encoded length of a proof with `m` digits.
#[must_use]
pub const fn encoded_len(m: usize) -> usize {
    1 + POINT_LEN * (4 + 2 * m) + SCALAR_LEN * (m * (BASE - 1) + 3)
}

/// The ring a proof is about: the keys as received and their decoded points.
#[derive(Clone, Copy)]
pub(crate) struct RingRef<'a> {
    pub(crate) keys: &'a [[u8; POINT_LEN]],
    pub(crate) points: &'a [EdwardsPoint],
}

/// A proof in its encoded form. Points and scalars are decoded and checked
/// only at verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    m: u8,
    a: [u8; POINT_LEN],
    b: [u8; POINT_LEN],
    c: [u8; POINT_LEN],
    d: [u8; POINT_LEN],
    x: Vec<[u8; POINT_LEN]>,
    y: Vec<[u8; POINT_LEN]>,
    f: Vec<[u8; SCALAR_LEN]>,
    z_a: [u8; SCALAR_LEN],
    z_c: [u8; SCALAR_LEN],
    z: [u8; SCALAR_LEN],
}

/// Wire layout: `m(1) || A || B || C || D || X_0..X_{m-1} || Y_0..Y_{m-1}
/// || f_{0,1}..f_{m-1,1} || z_A || z_C || z`, every point and scalar 32 bytes.
impl Proof {
    /// The number of digits.
    #[must_use]
    pub fn digits(&self) -> usize {
        usize::from(self.m)
    }

    /// Encode.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(encoded_len(self.digits()));
        out.push(self.m);
        for p in [&self.a, &self.b, &self.c, &self.d] {
            out.extend_from_slice(p);
        }
        for p in self.x.iter().chain(self.y.iter()) {
            out.extend_from_slice(p);
        }
        for s in &self.f {
            out.extend_from_slice(s);
        }
        for s in [&self.z_a, &self.z_c, &self.z] {
            out.extend_from_slice(s);
        }
        out
    }

    /// Decode, exact length only. Does not decode the points or scalars;
    /// [`verify`] does, and refuses any that are not canonical.
    #[allow(clippy::many_single_char_names)] // the papers' notation
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (&m, rest) = bytes.split_first().ok_or(BallotError::Malformed)?;
        let digits = usize::from(m);
        if digits == 0 || digits > MAX_DIGITS || bytes.len() != encoded_len(digits) {
            return Err(BallotError::Malformed);
        }
        let mut chunks = rest.chunks_exact(32).map(|c| {
            let mut out = [0u8; 32];
            out.copy_from_slice(c);
            out
        });
        let mut next = || chunks.next().ok_or(BallotError::Malformed);
        let a = next()?;
        let b = next()?;
        let c = next()?;
        let d = next()?;
        let x = (0..digits).map(|_| next()).collect::<Result<Vec<_>>>()?;
        let y = (0..digits).map(|_| next()).collect::<Result<Vec<_>>>()?;
        let f = (0..digits * (BASE - 1))
            .map(|_| next())
            .collect::<Result<Vec<_>>>()?;
        let z_a = next()?;
        let z_c = next()?;
        let z = next()?;
        Ok(Self {
            m,
            a,
            b,
            c,
            d,
            x,
            y,
            f,
            z_a,
            z_c,
            z,
        })
    }
}

/// The digit `j` of `index` in base [`BASE`].
fn digit(index: usize, j: usize) -> usize {
    let mut value = index;
    for _ in 0..j {
        value /= BASE;
    }
    value % BASE
}

fn ring_digest(keys: &[[u8; POINT_LEN]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(RING_DIGEST_DOMAIN);
    for key in keys {
        hasher.update(key);
    }
    *hasher.finalize().as_bytes()
}

/// The transcript up to, not including, the commitments.
fn statement_transcript(
    m: usize,
    ring: RingRef<'_>,
    context: &[u8],
    tag_base: &EdwardsPoint,
    tag: &[u8; POINT_LEN],
) -> Transcript {
    let mut t = Transcript::new(PROOF_DOMAIN);
    t.append_u64(b"base", BASE as u64);
    t.append_u64(b"digits", m as u64);
    t.append_u64(b"ring-len", ring.keys.len() as u64);
    t.append(b"ring", &ring_digest(ring.keys));
    t.append(b"context", context);
    t.append_point(b"tag-base", tag_base);
    t.append(b"tag", tag);
    t
}

/// Hedged prover randomness: an XOF keyed by fresh OS randomness, the secret
/// and the statement.
struct NonceStream {
    reader: blake3::OutputReader,
}

impl NonceStream {
    fn new(secret: &Scalar, statement: &[u8]) -> Self {
        let mut fresh = [0u8; 32];
        lakko_crypto::random::fill(&mut fresh);
        let mut secret_bytes = secret.to_bytes();
        let mut hasher = blake3::Hasher::new_derive_key(NONCE_DOMAIN);
        hasher.update(&fresh);
        hasher.update(&secret_bytes);
        hasher.update(statement);
        secret_bytes.zeroize();
        fresh.zeroize();
        Self {
            reader: hasher.finalize_xof(),
        }
    }

    fn scalar(&mut self) -> Scalar {
        let mut wide = [0u8; 64];
        self.reader.fill(&mut wide);
        let s = Scalar::from_bytes_mod_order_wide(&wide);
        wide.zeroize();
        s
    }
}

/// `p_k(X)` coefficients for every padded index `k`, flat with stride `m + 1`
/// (coefficient of `X^d` at `k * (m + 1) + d`), from the linear factors
/// `factor[j][i] = (s, a)` meaning `s X + a`.
fn polynomial_coefficients(m: usize, factors: &[Vec<(Scalar, Scalar)>]) -> Vec<Scalar> {
    let stride = m + 1;
    let mut current = vec![Scalar::ZERO; stride];
    current[0] = Scalar::ONE;
    let mut count = 1usize;
    for factor in factors.iter().take(m) {
        let mut next = vec![Scalar::ZERO; count * BASE * stride];
        for (i, (s, a)) in factor.iter().enumerate() {
            for k in 0..count {
                let src = &current[k * stride..(k + 1) * stride];
                let dst_start = (k + i * count) * stride;
                for d in 0..stride {
                    let mut value = a * src[d];
                    if d > 0 {
                        value += s * src[d - 1];
                    }
                    next[dst_start + d] = value;
                }
            }
        }
        current.zeroize();
        current = next;
        count *= BASE;
    }
    current
}

/// `prod_j f_{j,k_j}` for every padded index `k`, folded onto the unpadded
/// ring: indices at or past `len` are the repeated last key.
fn evaluated_products(m: usize, f: &[Vec<Scalar>], len: usize) -> Vec<Scalar> {
    let mut current = vec![Scalar::ONE];
    for row in f.iter().take(m) {
        let count = current.len();
        let mut next = vec![Scalar::ZERO; count * BASE];
        for (i, fi) in row.iter().enumerate() {
            for k in 0..count {
                next[k + i * count] = current[k] * fi;
            }
        }
        current = next;
    }
    fold_padding(current, len)
}

/// Fold the coefficients of padded indices `len..` onto `len - 1`.
fn fold_padding(mut padded: Vec<Scalar>, len: usize) -> Vec<Scalar> {
    if len == 0 || padded.len() <= len {
        return padded;
    }
    let extra: Scalar = padded[len..].iter().sum();
    // The prover's coefficients are secret; clear the tail before dropping it.
    for value in &mut padded[len..] {
        value.zeroize();
    }
    padded.truncate(len);
    padded[len - 1] += extra;
    padded
}

fn powers(x: &Scalar, up_to: usize) -> Vec<Scalar> {
    let mut out = Vec::with_capacity(up_to + 1);
    let mut acc = Scalar::ONE;
    for _ in 0..=up_to {
        out.push(acc);
        acc *= x;
    }
    out
}

/// Prove that the holder of `secret` owns ring key `index` and that
/// `tag = secret^-1 tag_base`, bound to `context`.
///
/// Constant time in the secret and the index as far as curve25519-dalek's
/// constant-time multiscalar multiplication and scalar arithmetic are; the
/// loops depend only on public sizes.
// The names and the step order follow the papers' notation, which a
// reviewer checks this function against.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub(crate) fn prove(
    ring: RingRef<'_>,
    index: usize,
    secret: &Scalar,
    tag_base: &EdwardsPoint,
    tag: &EdwardsPoint,
    context: &[u8],
) -> Result<Proof> {
    let len = ring.keys.len();
    if ring.points.len() != len || index >= len {
        return Err(BallotError::NotInRing);
    }
    if ring.points[index] != crate::point::G * secret {
        return Err(BallotError::WitnessMismatch);
    }
    let m = digits_for(len)?;
    let m_byte = u8::try_from(m).map_err(|_| BallotError::RingTooLarge)?;
    let (h, gens) = commitment_generators(m * BASE)?;
    let tag_bytes = tag.compress().to_bytes();

    let mut transcript = statement_transcript(m, ring, context, tag_base, &tag_bytes);
    let mut statement = ring_digest(ring.keys).to_vec();
    statement.extend_from_slice(context);
    statement.extend_from_slice(&tag_bytes);
    let mut nonces = NonceStream::new(secret, &statement);

    let mut r_a = nonces.scalar();
    let mut r_b = nonces.scalar();
    let mut r_c = nonces.scalar();
    let mut r_d = nonces.scalar();
    let mut a: Vec<Vec<Scalar>> = Vec::with_capacity(m);
    let mut sigma: Vec<Vec<Scalar>> = Vec::with_capacity(m);
    for j in 0..m {
        let mut row = vec![Scalar::ZERO; BASE];
        let mut sum = Scalar::ZERO;
        for value in row.iter_mut().skip(1) {
            *value = nonces.scalar();
            sum += *value;
        }
        row[0] = -sum;
        a.push(row);
        let l_j = digit(index, j);
        sigma.push(
            (0..BASE)
                .map(|i| Scalar::from(u64::from(l_j == i)))
                .collect(),
        );
    }
    let mut rho: Vec<Scalar> = (0..m).map(|_| nonces.scalar()).collect();

    let mut a_flat: Vec<Scalar> = a.iter().flatten().copied().collect();
    let mut sigma_flat: Vec<Scalar> = sigma.iter().flatten().copied().collect();
    let commit = |blind: &Scalar, values: &[Scalar]| -> EdwardsPoint {
        EdwardsPoint::multiscalar_mul(
            core::iter::once(blind).chain(values.iter()),
            core::iter::once(&h).chain(gens.iter()),
        )
    };
    let mut c_values: Vec<Scalar> = a_flat
        .iter()
        .zip(&sigma_flat)
        .map(|(a, s)| a * (Scalar::ONE - s - s))
        .collect();
    let mut d_values: Vec<Scalar> = a_flat.iter().map(|a| -(a * a)).collect();
    let big_a = commit(&r_a, &a_flat).compress().to_bytes();
    let big_b = commit(&r_b, &sigma_flat).compress().to_bytes();
    let big_c = commit(&r_c, &c_values).compress().to_bytes();
    let big_d = commit(&r_d, &d_values).compress().to_bytes();
    for v in [&mut a_flat, &mut sigma_flat, &mut c_values, &mut d_values] {
        v.zeroize();
    }

    let mut factors: Vec<Vec<(Scalar, Scalar)>> = sigma
        .iter()
        .zip(&a)
        .map(|(s_row, a_row)| s_row.iter().copied().zip(a_row.iter().copied()).collect())
        .collect();
    let mut coefficients = polynomial_coefficients(m, &factors);
    for (s, a) in factors.iter_mut().flatten() {
        s.zeroize();
        a.zeroize();
    }
    let stride = m + 1;
    let padded = capacity(m);
    let mut big_x = Vec::with_capacity(m);
    let mut big_y = Vec::with_capacity(m);
    for (j, rho_j) in rho.iter().enumerate() {
        let column: Vec<Scalar> = (0..padded).map(|k| coefficients[k * stride + j]).collect();
        let mut column = fold_padding(column, len);
        let x_j = EdwardsPoint::multiscalar_mul(
            column.iter().chain(core::iter::once(rho_j)),
            ring.points.iter().chain(core::iter::once(&crate::point::G)),
        );
        column.zeroize();
        big_x.push(x_j.compress().to_bytes());
        big_y.push((tag * rho_j).compress().to_bytes());
    }
    coefficients.zeroize();

    for (label, p) in [
        (b"A", &big_a),
        (b"B", &big_b),
        (b"C", &big_c),
        (b"D", &big_d),
    ] {
        transcript.append(label, p);
    }
    for p in &big_x {
        transcript.append(b"X", p);
    }
    for p in &big_y {
        transcript.append(b"Y", p);
    }
    let x = transcript.challenge(b"x");
    let x_powers = powers(&x, m);

    let mut f = Vec::with_capacity(m * (BASE - 1));
    for j in 0..m {
        for i in 1..BASE {
            f.push((sigma[j][i] * x + a[j][i]).to_bytes());
        }
    }
    let z_a = (r_a + x * r_b).to_bytes();
    let z_c = (x * r_c + r_d).to_bytes();
    let mut z = secret * x_powers[m];
    for (j, rho_j) in rho.iter().enumerate() {
        z -= rho_j * x_powers[j];
    }
    let z = z.to_bytes();

    r_a.zeroize();
    r_b.zeroize();
    r_c.zeroize();
    r_d.zeroize();
    for row in a.iter_mut().chain(sigma.iter_mut()) {
        row.zeroize();
    }
    rho.zeroize();

    Ok(Proof {
        m: m_byte,
        a: big_a,
        b: big_b,
        c: big_c,
        d: big_d,
        x: big_x,
        y: big_y,
        f,
        z_a,
        z_c,
        z,
    })
}

/// Verify a proof against a ring, tag base, tag and context. Variable time:
/// every input is public.
// The papers' notation.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub(crate) fn verify(
    ring: RingRef<'_>,
    tag_base: &EdwardsPoint,
    tag_bytes: &[u8; POINT_LEN],
    tag: &EdwardsPoint,
    proof: &Proof,
    context: &[u8],
) -> Result<()> {
    let len = ring.keys.len();
    if ring.points.len() != len {
        return Err(BallotError::RingVersionUnusable);
    }
    let m = digits_for(len)?;
    if proof.digits() != m
        || proof.x.len() != m
        || proof.y.len() != m
        || proof.f.len() != m * (BASE - 1)
    {
        return Err(BallotError::ProofRejected);
    }
    let reject = |_| BallotError::ProofRejected;
    let big_a = decode_point(&proof.a).map_err(reject)?;
    let big_b = decode_point(&proof.b).map_err(reject)?;
    let big_c = decode_point(&proof.c).map_err(reject)?;
    let big_d = decode_point(&proof.d).map_err(reject)?;
    let big_x = proof
        .x
        .iter()
        .map(decode_point)
        .collect::<Result<Vec<_>>>()
        .map_err(reject)?;
    let big_y = proof
        .y
        .iter()
        .map(decode_point)
        .collect::<Result<Vec<_>>>()
        .map_err(reject)?;
    let f_sent = proof
        .f
        .iter()
        .map(decode_scalar)
        .collect::<Result<Vec<_>>>()
        .map_err(reject)?;
    let z_a = decode_scalar(&proof.z_a).map_err(reject)?;
    let z_c = decode_scalar(&proof.z_c).map_err(reject)?;
    let z = decode_scalar(&proof.z).map_err(reject)?;

    let mut transcript = statement_transcript(m, ring, context, tag_base, tag_bytes);
    for (label, p) in [
        (b"A", &proof.a),
        (b"B", &proof.b),
        (b"C", &proof.c),
        (b"D", &proof.d),
    ] {
        transcript.append(label, p);
    }
    for p in &proof.x {
        transcript.append(b"X", p);
    }
    for p in &proof.y {
        transcript.append(b"Y", p);
    }
    let x = transcript.challenge(b"x");
    let x_powers = powers(&x, m);

    let mut f: Vec<Vec<Scalar>> = Vec::with_capacity(m);
    for j in 0..m {
        let sent = &f_sent[j * (BASE - 1)..(j + 1) * (BASE - 1)];
        let sum: Scalar = sent.iter().sum();
        let mut row = Vec::with_capacity(BASE);
        row.push(x - sum);
        row.extend_from_slice(sent);
        f.push(row);
    }
    let f_flat: Vec<Scalar> = f.iter().flatten().copied().collect();
    let (h, gens) = commitment_generators(m * BASE)?;

    // A + x B - z_A H - sum f G == 0
    let check_ab = EdwardsPoint::vartime_multiscalar_mul(
        [Scalar::ONE, x, -z_a]
            .iter()
            .chain(f_flat.iter().map(|v| -v).collect::<Vec<_>>().iter()),
        [big_a, big_b, h].iter().chain(gens.iter()),
    );
    // x C + D - z_C H - sum f (x - f) G == 0
    let fxf: Vec<Scalar> = f_flat.iter().map(|v| -(v * (x - v))).collect();
    let check_cd = EdwardsPoint::vartime_multiscalar_mul(
        [x, Scalar::ONE, -z_c].iter().chain(fxf.iter()),
        [big_c, big_d, h].iter().chain(gens.iter()),
    );
    // sum_k p_k M_k - sum_j x^j X_j - z G == 0
    let products = evaluated_products(m, &f, len);
    let neg_x_powers: Vec<Scalar> = x_powers[..m].iter().map(|v| -v).collect();
    let check_ring = EdwardsPoint::vartime_multiscalar_mul(
        products
            .iter()
            .chain(neg_x_powers.iter())
            .chain(core::iter::once(&-z)),
        ring.points
            .iter()
            .chain(big_x.iter())
            .chain(core::iter::once(&crate::point::G)),
    );
    // x^m U - sum_j x^j Y_j - z J == 0
    let check_tag = EdwardsPoint::vartime_multiscalar_mul(
        core::iter::once(&x_powers[m])
            .chain(neg_x_powers.iter())
            .chain(core::iter::once(&-z)),
        core::iter::once(tag_base)
            .chain(big_y.iter())
            .chain(core::iter::once(tag)),
    );

    if check_ab.is_identity()
        && check_cd.is_identity()
        && check_ring.is_identity()
        && check_tag.is_identity()
    {
        Ok(())
    } else {
        Err(BallotError::ProofRejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::{hash_to_point, G};

    fn ring_of(n: usize) -> (Vec<Scalar>, Vec<[u8; 32]>, Vec<EdwardsPoint>) {
        let secrets: Vec<Scalar> = (0..n)
            .map(|i| {
                Scalar::from_bytes_mod_order_wide(&[u8::try_from(i % 251).unwrap() + 1; 64])
                    + Scalar::from(i as u64)
            })
            .collect();
        let points: Vec<EdwardsPoint> = secrets.iter().map(|s| G * s).collect();
        let keys = points.iter().map(|p| p.compress().to_bytes()).collect();
        (secrets, keys, points)
    }

    #[test]
    fn digits_for_matches_powers_of_two() {
        assert_eq!(digits_for(1).unwrap(), 1);
        assert_eq!(digits_for(2).unwrap(), 1);
        assert_eq!(digits_for(3).unwrap(), 2);
        assert_eq!(digits_for(4).unwrap(), 2);
        assert_eq!(digits_for(5).unwrap(), 3);
        assert_eq!(digits_for(1 << MAX_DIGITS).unwrap(), MAX_DIGITS);
        assert_eq!(
            digits_for((1 << MAX_DIGITS) + 1),
            Err(BallotError::RingTooLarge)
        );
        assert_eq!(digits_for(0), Err(BallotError::InvalidRing));
    }

    #[allow(clippy::many_single_char_names)]
    #[test]
    fn completeness_at_every_position_for_small_rings() {
        let u = hash_to_point("lakko/test/tag-base/v1", b"poll").unwrap();
        for n in 1..=9 {
            let (secrets, keys, points) = ring_of(n);
            let ring = RingRef {
                keys: &keys,
                points: &points,
            };
            for (l, secret) in secrets.iter().enumerate() {
                let tag = u * secret.invert();
                let proof = prove(ring, l, secret, &u, &tag, b"ctx").unwrap();
                assert_eq!(proof.encode().len(), encoded_len(digits_for(n).unwrap()));
                let decoded = Proof::decode(&proof.encode()).unwrap();
                let tag_bytes = tag.compress().to_bytes();
                verify(ring, &u, &tag_bytes, &tag, &decoded, b"ctx").unwrap();
                assert_eq!(
                    verify(ring, &u, &tag_bytes, &tag, &decoded, b"other"),
                    Err(BallotError::ProofRejected)
                );
            }
        }
    }

    #[test]
    fn wrong_witness_is_refused_at_prove_time() {
        let (secrets, keys, points) = ring_of(4);
        let ring = RingRef {
            keys: &keys,
            points: &points,
        };
        let u = hash_to_point("lakko/test/tag-base/v1", b"poll").unwrap();
        let tag = u * secrets[1].invert();
        assert_eq!(
            prove(ring, 0, &secrets[1], &u, &tag, b""),
            Err(BallotError::WitnessMismatch)
        );
    }

    #[test]
    fn a_tag_of_another_secret_does_not_verify() {
        let (secrets, keys, points) = ring_of(4);
        let ring = RingRef {
            keys: &keys,
            points: &points,
        };
        let u = hash_to_point("lakko/test/tag-base/v1", b"poll").unwrap();
        // Honest member 0 tries to attach member 1's tag.
        let wrong_tag = u * secrets[1].invert();
        let proof = prove(ring, 0, &secrets[0], &u, &wrong_tag, b"").unwrap();
        let tag_bytes = wrong_tag.compress().to_bytes();
        assert_eq!(
            verify(ring, &u, &tag_bytes, &wrong_tag, &proof, b""),
            Err(BallotError::ProofRejected)
        );
    }
}
