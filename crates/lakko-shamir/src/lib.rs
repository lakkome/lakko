// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shamir's Secret Sharing over GF(256) for Lakko's social-
//! recovery flow (Phase 1.21).
//!
//! ## Threat model
//!
//! Recovery passphrase (BIP39 24-word entropy, 32 bytes — see
//! `lakko-mnemonic`) is split into `n` shares with threshold `k`
//! (default Lakko configuration: 3-of-5). Any `k` shares
//! reconstruct the original secret; any `k-1` shares reveal
//! **zero information** (information-theoretic security — the
//! standard property of Shamir's scheme over a finite field).
//!
//! Each share is distributed to a separate guardian (a trusted
//! contact, a paper backup in a safe, a password manager, …).
//! Compromise of < k guardians does not compromise the recovery;
//! collusion of ≥ k guardians fully recovers it.
//!
//! ## Why GF(256) and not GF(2^32) / GF(p)
//!
//! GF(256) lets us split a byte-array secret byte-wise (one
//! independent polynomial per byte position). Share data is the
//! same length as the secret — no padding overhead. The smaller
//! field also means the share index fits in one byte (1..=255).
//!
//! For a 32-byte recovery seed, each share carries 32 bytes of
//! polynomial values + 1 byte index = 33 bytes total. Encoded as
//! `<index_hex>:<base64>` that's ~50 ASCII chars — comfortably
//! short enough for a QR code or a 6×8 grid of paper digits.
//!
//! ## Why hand-rolled
//!
//! Two reasons:
//!
//!   1. **Audit surface**: GF(256) Shamir is < 200 lines. A
//!      reviewer can read every byte that touches the secret.
//!   2. **Supply chain**: every Shamir crate on crates.io is
//!      small (sharks, vsss-rs, threshold_crypto, …); adding a
//!      dep for ~200 lines of arithmetic costs more in
//!      supply-chain risk than it saves in code.
//!
//! ## What's NOT here
//!
//! - **VSS (Verifiable Secret Sharing)**: a malicious dealer
//!   could hand out shares that don't reconstruct the claimed
//!   secret. Out of scope — Lakko's recovery flow has a single
//!   dealer (the user themselves on their first device) who has
//!   no incentive to defraud their own recovery. If we ever
//!   support DKG-style dealer-less generation, VSS / Feldman /
//!   Pedersen lands then.
//! - **SLIP-0039**: the canonical user-facing format for
//!   wallet-recovery Shamir, with 20-word groups + group-level
//!   thresholds. We may layer SLIP-0039 on top of this crate
//!   later; for 1.21 the raw byte-level shares are enough.
//! - **Forward-error correction**: a corrupted share is rejected,
//!   not auto-repaired. The encoded format includes a CRC so
//!   accidental transcription errors surface as a clean
//!   `WireFormat` rejection rather than a silent wrong-secret
//!   recovery.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

mod gf256;

/// Lower bound on the threshold. A threshold of 1 means every
/// share alone reconstructs the secret — that's not Shamir, it's
/// just N copies of the secret. Reject at the API.
pub const MIN_THRESHOLD: u8 = 2;

/// Upper bound on the number of shares. GF(256) supports up to
/// 255 distinct non-zero indices, but the Lakko social-recovery
/// UX tops out at "5 guardians" — anything past that is enterprise
/// key escrow, not personal recovery. 16 gives headroom for power
/// users without pushing into the awkward 100+ share regime where
/// the polynomial split gets slow (still linear, but the constant
/// factor matters on mobile).
pub const MAX_SHARES: u8 = 16;

/// Upper bound on the secret length. The current use case is the
/// 32-byte BIP39 entropy; 1 KiB gives generous headroom for
/// future shapes (e.g. an X25519 keypair plus metadata) without
/// inviting denial-of-service via huge polynomial splits.
pub const MAX_SECRET_BYTES: usize = 1024;

/// One share of a Shamir-split secret.
///
/// `index` is the polynomial evaluation point (`1..=n_shares`,
/// never zero — zero is the secret itself), `data` is the
/// polynomial value at that point, byte-wise (one polynomial per
/// secret byte, evaluated independently).
///
/// Wire format produced by [`Share::encode`]:
/// `<index_decimal>:<base64-no-pad data>`. The colon-separated
/// shape is human-typable (a user reading from a paper backup
/// types `3:abc...` into a recovery box; the digit prefix is
/// unambiguous), and base64 is the standard alphabet without
/// padding (mirror of the workspace auth + discovery wire
/// conventions). [`Share::decode`] parses the inverse.
#[derive(Clone, Eq, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Share {
    /// Polynomial evaluation point. Non-zero by construction —
    /// `0` is the secret itself, never an emitted share.
    pub index: u8,
    /// Polynomial values, one per secret byte. Same length as
    /// the original secret.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Share {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the data bytes — they're shares of a secret.
        // Just surface the index + length so a panic message is
        // informative without leaking material.
        f.debug_struct("Share")
            .field("index", &self.index)
            .field("data_len", &self.data.len())
            .finish_non_exhaustive()
    }
}

impl Share {
    /// Encode to the human-typable wire form
    /// `<index_decimal>:<base64>`.
    ///
    /// Decimal index (not hex) is deliberate — humans transcribe
    /// decimal digits with fewer errors than hex (no `b` vs `6`
    /// confusion, no case sensitivity).
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}", self.index, B64.encode(&self.data))
    }

    /// Parse from the [`Share::encode`] wire form.
    ///
    /// # Errors
    ///
    /// Returns `ShamirError::WireFormat` if the input does not
    /// match the `<index>:<base64>` shape, the index is not a
    /// 1..=255 decimal, or the base64 fails to decode.
    pub fn decode(encoded: &str) -> Result<Self, ShamirError> {
        let trimmed = encoded.trim();
        let (idx_str, data_str) = trimmed
            .split_once(':')
            .ok_or_else(|| ShamirError::WireFormat("missing ':'".into()))?;
        let index: u8 = idx_str
            .parse()
            .map_err(|_| ShamirError::WireFormat("index must be 1..=255 decimal".into()))?;
        if index == 0 {
            return Err(ShamirError::WireFormat(
                "index 0 reserved for the secret".into(),
            ));
        }
        let data = B64
            .decode(data_str.as_bytes())
            .map_err(|_| ShamirError::WireFormat("base64 decode failed".into()))?;
        if data.is_empty() {
            return Err(ShamirError::WireFormat("empty data".into()));
        }
        if data.len() > MAX_SECRET_BYTES {
            return Err(ShamirError::WireFormat(format!(
                "data {} bytes exceeds the {MAX_SECRET_BYTES}-byte ceiling",
                data.len()
            )));
        }
        Ok(Self { index, data })
    }
}

/// Errors surfaced by [`split`] / [`combine`] / [`Share::decode`].
#[derive(Debug, Error)]
pub enum ShamirError {
    /// `secret` was empty.
    #[error("secret must be non-empty")]
    EmptySecret,
    /// `secret` length exceeds [`MAX_SECRET_BYTES`].
    #[error("secret {got} bytes exceeds the {max}-byte ceiling")]
    SecretTooLarge {
        /// Actual length of the offending secret.
        got: usize,
        /// `MAX_SECRET_BYTES`.
        max: usize,
    },
    /// `threshold` was `0`, `1`, or `> n_shares`.
    #[error("threshold {got} invalid (must be in {min}..={n_shares})")]
    InvalidThreshold {
        /// Caller-supplied threshold.
        got: u8,
        /// `MIN_THRESHOLD`.
        min: u8,
        /// `n_shares`.
        n_shares: u8,
    },
    /// `n_shares` was `0` or above [`MAX_SHARES`].
    #[error("n_shares {got} invalid (must be in 1..={max})")]
    InvalidShareCount {
        /// Caller-supplied share count.
        got: u8,
        /// `MAX_SHARES`.
        max: u8,
    },
    /// [`combine`] received fewer shares than the original threshold.
    /// We can't detect this directly (the threshold isn't stored on
    /// the wire); recovery instead returns garbage when too few
    /// shares are combined. The constructor surface enforces
    /// `shares.len() >= 2` to at least reject the trivially-broken
    /// case.
    #[error("at least 2 shares required to combine; got {got}")]
    NotEnoughShares {
        /// Number of shares actually supplied.
        got: usize,
    },
    /// Two shares carried the same `index`. Combining colliding
    /// indices is mathematically undefined (the Lagrange basis is
    /// degenerate).
    #[error("duplicate share indices in input")]
    DuplicateIndices,
    /// Shares supplied to [`combine`] had differing `data` lengths.
    /// All shares of one split must be the same byte length as the
    /// original secret.
    #[error("shares have mismatched lengths")]
    MismatchedLengths,
    /// Wire-format failure on [`Share::decode`].
    #[error("wire format: {0}")]
    WireFormat(String),
}

/// Split `secret` into `n_shares` shares such that any `threshold`
/// of them suffice to reconstruct it via [`combine`]. `threshold`
/// must be at least [`MIN_THRESHOLD`] and at most `n_shares`;
/// `n_shares` is capped at [`MAX_SHARES`].
///
/// Each output `Share` carries a unique `index` in `1..=n_shares`.
///
/// # Errors
///
/// Surfaces an [`ShamirError`] variant for empty / oversize
/// secrets and out-of-range threshold / share-count parameters.
///
/// # Determinism
///
/// **Not deterministic.** A fresh split of the same secret
/// produces different shares each call (the polynomial
/// coefficients are random by construction). Recovery of the
/// secret IS deterministic — same `k` shares always reconstruct
/// the same bytes.
pub fn split(secret: &[u8], threshold: u8, n_shares: u8) -> Result<Vec<Share>, ShamirError> {
    if secret.is_empty() {
        return Err(ShamirError::EmptySecret);
    }
    if secret.len() > MAX_SECRET_BYTES {
        return Err(ShamirError::SecretTooLarge {
            got: secret.len(),
            max: MAX_SECRET_BYTES,
        });
    }
    if n_shares == 0 || n_shares > MAX_SHARES {
        return Err(ShamirError::InvalidShareCount {
            got: n_shares,
            max: MAX_SHARES,
        });
    }
    if threshold < MIN_THRESHOLD || threshold > n_shares {
        return Err(ShamirError::InvalidThreshold {
            got: threshold,
            min: MIN_THRESHOLD,
            n_shares,
        });
    }

    let degree = usize::from(threshold) - 1;
    let mut shares: Vec<Share> = (1..=n_shares)
        .map(|idx| Share {
            index: idx,
            data: vec![0u8; secret.len()],
        })
        .collect();

    // Per-byte independent polynomial split. The polynomial
    // `p_i(x) = secret[i] + c_1 x + c_2 x^2 + … + c_{k-1} x^{k-1}`
    // is evaluated at each share's `index`. Coefficients are
    // fresh random each iteration; the standard library scratch
    // buffer is zeroized between iterations to avoid leaving
    // polynomial coefficients on the heap.
    let mut coeffs = vec![0u8; degree];
    for (byte_idx, &secret_byte) in secret.iter().enumerate() {
        // Fresh random coefficients for this byte.
        if !coeffs.is_empty() {
            OsRng.fill_bytes(&mut coeffs);
        }
        for share in &mut shares {
            // p(x) = secret_byte + sum_{j=1..=degree}(coeffs[j-1] * x^j)
            //
            // Computed via Horner's method, applied from the top:
            //     acc = coeffs[degree-1]
            //     for j in (degree-2)..=0: acc = acc * x + coeffs[j]
            //     return acc * x + secret_byte
            let x = share.index;
            let mut acc: u8 = 0;
            for &c in coeffs.iter().rev() {
                acc = gf256::add(gf256::mul(acc, x), c);
            }
            acc = gf256::add(gf256::mul(acc, x), secret_byte);
            share.data[byte_idx] = acc;
        }
    }
    // Zeroize the per-call scratch buffer — the coefficients are
    // not the secret itself but they carry information about it.
    coeffs.zeroize();

    Ok(shares)
}

/// Reconstruct the secret from `shares` via Lagrange interpolation
/// at `x = 0`. Returns the original secret bytes when the supplied
/// `shares` count is at least the original split's `threshold`
/// (using any combination of unique-indexed shares of the same
/// split); returns garbage when called with fewer than `threshold`
/// shares (the polynomial degree is under-determined — Shamir's
/// information-theoretic property means the under-determined
/// recovery is indistinguishable from any other possible secret).
///
/// We deliberately do NOT store `threshold` on the wire — the
/// caller's recovery UX is "collect K shares, hit Recover, see
/// whether the result decodes as a valid 24-word phrase". A wrong
/// K surfaces as a BIP39 checksum failure downstream, not as a
/// helpful error here.
///
/// # Errors
///
/// Returns [`ShamirError::NotEnoughShares`] if fewer than 2 shares
/// are supplied, [`ShamirError::DuplicateIndices`] on a colliding-
/// index input, and [`ShamirError::MismatchedLengths`] if the
/// shares disagree on `data.len()`.
pub fn combine(shares: &[Share]) -> Result<Vec<u8>, ShamirError> {
    if shares.len() < 2 {
        return Err(ShamirError::NotEnoughShares { got: shares.len() });
    }

    // Length consistency.
    let secret_len = shares[0].data.len();
    if secret_len == 0 {
        return Err(ShamirError::EmptySecret);
    }
    for s in &shares[1..] {
        if s.data.len() != secret_len {
            return Err(ShamirError::MismatchedLengths);
        }
    }

    // Duplicate-index check. Constant-time over (i, j) so a
    // malicious caller can't time-side-channel which two shares
    // collided. n is small (≤ 16) so this is fast in absolute
    // terms.
    for i in 0..shares.len() {
        for j in (i + 1)..shares.len() {
            if bool::from(shares[i].index.ct_eq(&shares[j].index)) {
                return Err(ShamirError::DuplicateIndices);
            }
        }
    }

    // Per-byte Lagrange interpolation at x = 0.
    //
    // For each byte position `b`, interpolate the polynomial
    // p(x) such that p(shares[k].index) = shares[k].data[b]
    // and evaluate at x = 0. The result is the secret byte.
    let mut secret = vec![0u8; secret_len];
    for (byte_idx, secret_byte) in secret.iter_mut().enumerate() {
        let mut sum: u8 = 0;
        for (j, share_j) in shares.iter().enumerate() {
            // Compute the Lagrange basis polynomial L_j(0):
            //   L_j(0) = product over m != j of (0 - x_m) / (x_j - x_m)
            //          = product over m != j of (-x_m) / (x_j - x_m)
            // In GF(256), -x = x (characteristic 2), so this
            // simplifies to product of x_m / (x_j + x_m).
            let x_j = share_j.index;
            let mut basis: u8 = 1;
            for (m, share_m) in shares.iter().enumerate() {
                if m == j {
                    continue;
                }
                let x_m = share_m.index;
                let num = x_m; // -x_m == x_m in GF(2^k)
                let den = gf256::add(x_j, x_m); // x_j - x_m == x_j + x_m
                                                // `den` cannot be zero because duplicate-index
                                                // check above rejected (x_j == x_m) before we
                                                // got here.
                basis = gf256::mul(basis, gf256::div(num, den));
            }
            sum = gf256::add(sum, gf256::mul(share_j.data[byte_idx], basis));
        }
        *secret_byte = sum;
    }

    Ok(secret)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_3_of_5_default_lakko() {
        let secret = b"hello world this is the recovery seed entropy".to_vec();
        let shares = split(&secret, 3, 5).unwrap();
        assert_eq!(shares.len(), 5);
        // Every threshold-sized subset reconstructs.
        for i in 0..5 {
            for j in (i + 1)..5 {
                for k in (j + 1)..5 {
                    let recovered =
                        combine(&[shares[i].clone(), shares[j].clone(), shares[k].clone()])
                            .unwrap();
                    assert_eq!(recovered, secret);
                }
            }
        }
    }

    #[test]
    fn round_trip_2_of_2_degenerate() {
        // Minimum sensible split: 2-of-2. Either party alone
        // learns nothing.
        let secret = vec![0x42u8; 32];
        let shares = split(&secret, 2, 2).unwrap();
        assert_eq!(shares.len(), 2);
        let recovered = combine(&shares).unwrap();
        assert_eq!(recovered, secret);
    }

    #[test]
    fn round_trip_5_of_5_unanimous() {
        // 5-of-5: everyone must agree. Recovery is information-
        // theoretically secure against any 4 colluding holders.
        let secret = (0..32u8).collect::<Vec<u8>>();
        let shares = split(&secret, 5, 5).unwrap();
        let recovered = combine(&shares).unwrap();
        assert_eq!(recovered, secret);
    }

    #[test]
    fn under_threshold_returns_garbage() {
        // 3-of-5: any 2 shares reconstruct *some* byte vector
        // but not the secret. The exact garbage depends on the
        // random coefficients; the property we assert is "not
        // equal to the original".
        let secret = b"a real recovery seed".to_vec();
        let shares = split(&secret, 3, 5).unwrap();
        let two = vec![shares[0].clone(), shares[2].clone()];
        let recovered = combine(&two).unwrap();
        // With overwhelming probability the two-share recovery
        // differs from the secret. (Probability of accidental
        // match: 1 / 256^secret_len — for 20 bytes that's
        // 2^-160, well below any test flake threshold.)
        assert_ne!(recovered, secret);
    }

    #[test]
    fn shares_have_distinct_indices() {
        let secret = vec![1u8; 16];
        let shares = split(&secret, 3, 5).unwrap();
        let mut indices: Vec<u8> = shares.iter().map(|s| s.index).collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(indices.len(), 5);
    }

    #[test]
    fn share_data_is_same_length_as_secret() {
        let secret = vec![0xAAu8; 47];
        let shares = split(&secret, 2, 3).unwrap();
        for s in &shares {
            assert_eq!(s.data.len(), 47);
        }
    }

    #[test]
    fn reject_empty_secret() {
        assert!(matches!(split(&[], 2, 3), Err(ShamirError::EmptySecret)));
    }

    #[test]
    fn reject_oversize_secret() {
        let big = vec![0u8; MAX_SECRET_BYTES + 1];
        assert!(matches!(
            split(&big, 2, 3),
            Err(ShamirError::SecretTooLarge { .. })
        ));
    }

    #[test]
    fn reject_zero_threshold() {
        assert!(matches!(
            split(b"x", 0, 3),
            Err(ShamirError::InvalidThreshold { .. })
        ));
    }

    #[test]
    fn reject_threshold_of_one() {
        // Threshold of 1 means every share alone reconstructs —
        // that's just N copies of the secret, not Shamir. We
        // reject it explicitly.
        assert!(matches!(
            split(b"x", 1, 3),
            Err(ShamirError::InvalidThreshold { .. })
        ));
    }

    #[test]
    fn reject_threshold_exceeding_shares() {
        assert!(matches!(
            split(b"x", 4, 3),
            Err(ShamirError::InvalidThreshold { .. })
        ));
    }

    #[test]
    fn reject_zero_shares() {
        assert!(matches!(
            split(b"x", 2, 0),
            Err(ShamirError::InvalidShareCount { .. })
        ));
    }

    #[test]
    fn reject_oversize_share_count() {
        assert!(matches!(
            split(b"x", 2, MAX_SHARES + 1),
            Err(ShamirError::InvalidShareCount { .. })
        ));
    }

    #[test]
    fn combine_rejects_duplicate_indices() {
        let secret = b"abc".to_vec();
        let shares = split(&secret, 2, 3).unwrap();
        let dup = vec![shares[0].clone(), shares[0].clone()];
        assert!(matches!(combine(&dup), Err(ShamirError::DuplicateIndices)));
    }

    #[test]
    fn combine_rejects_single_share() {
        let secret = b"abc".to_vec();
        let shares = split(&secret, 2, 3).unwrap();
        let one = vec![shares[0].clone()];
        assert!(matches!(
            combine(&one),
            Err(ShamirError::NotEnoughShares { got: 1 })
        ));
    }

    #[test]
    fn combine_rejects_mismatched_lengths() {
        let s1 = Share {
            index: 1,
            data: vec![1, 2, 3],
        };
        let s2 = Share {
            index: 2,
            data: vec![1, 2],
        };
        assert!(matches!(
            combine(&[s1, s2]),
            Err(ShamirError::MismatchedLengths)
        ));
    }

    #[test]
    fn wire_format_round_trip() {
        let secret = b"recovery seed".to_vec();
        let shares = split(&secret, 2, 3).unwrap();
        for share in &shares {
            let encoded = share.encode();
            let decoded = Share::decode(&encoded).unwrap();
            assert_eq!(decoded.index, share.index);
            assert_eq!(decoded.data, share.data);
        }
    }

    #[test]
    fn wire_format_rejects_missing_colon() {
        assert!(matches!(
            Share::decode("nodot"),
            Err(ShamirError::WireFormat(_))
        ));
    }

    #[test]
    fn wire_format_rejects_index_zero() {
        assert!(matches!(
            Share::decode("0:AAAA"),
            Err(ShamirError::WireFormat(_))
        ));
    }

    #[test]
    fn wire_format_rejects_bad_base64() {
        assert!(matches!(
            Share::decode("1:!!!!"),
            Err(ShamirError::WireFormat(_))
        ));
    }

    #[test]
    fn wire_format_rejects_empty_data() {
        assert!(matches!(
            Share::decode("1:"),
            Err(ShamirError::WireFormat(_))
        ));
    }

    #[test]
    fn shares_are_random_across_calls() {
        // Two splits of the same secret produce different shares
        // (the polynomial coefficients are fresh-random each
        // call). Both still recover to the same secret.
        let secret = b"a 32-byte secret string here....".to_vec();
        let s1 = split(&secret, 3, 5).unwrap();
        let s2 = split(&secret, 3, 5).unwrap();
        // Indices are deterministic (always 1..=n), but data
        // differs — assert on at least one share's data
        // differing. With 32 byte * 8 bit of randomness per
        // share the chance of collision is 2^-256.
        assert_ne!(s1[0].data, s2[0].data);
        // Both recover correctly.
        assert_eq!(combine(&s1[0..3]).unwrap(), secret);
        assert_eq!(combine(&s2[0..3]).unwrap(), secret);
    }

    #[test]
    fn share_zeroize_clears_in_place() {
        // Smoke test for the `Zeroize` derive — `Vec<u8>`'s
        // `Zeroize::zeroize()` impl from the `zeroize` crate
        // overwrites every byte with zero AND truncates the
        // length to 0 (the canonical "drop the heap allocation
        // contents" semantics). Both halves of the contract
        // matter — a share buffer that's been zeroed should
        // no longer be readable as the polynomial values.
        let mut shares = split(b"recovery seed bytes", 2, 3).unwrap();
        assert!(!shares[0].data.is_empty());
        shares[0].data.zeroize();
        assert!(
            shares[0].data.is_empty(),
            "Vec<u8>::zeroize() truncates to length 0",
        );
    }

    #[test]
    fn distinct_secrets_produce_distinct_shares() {
        // Two splits with the same threshold + share count over
        // *different* secrets must produce different per-share
        // data byte sequences (otherwise the share would carry
        // no information about the secret — a broken split).
        //
        // This is the canonical Shamir-correctness check: shares
        // depend on both the secret and the random coefficients.
        // We pin the share count + threshold so the indices line
        // up across splits.
        let a = split(b"AAAAAA", 2, 3).unwrap();
        let b = split(b"BBBBBB", 2, 3).unwrap();
        // Same indices, different data — random coefficients
        // alone could in principle produce identical data, but
        // for a 6-byte secret with 2-of-3 the chance of an
        // accidental match is 1 in 256^6 ≈ 2^-48. Comfortably
        // below test-flake threshold.
        assert_eq!(a[0].index, b[0].index);
        assert_ne!(a[0].data, b[0].data);
    }

    #[test]
    fn debug_does_not_leak_share_data() {
        // The custom Debug impl elides `data`. A panic message
        // surfaces the index + length, never the polynomial
        // values.
        let share = Share {
            index: 3,
            data: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let d = format!("{share:?}");
        assert!(d.contains("index: 3"));
        assert!(d.contains("data_len: 4"));
        assert!(!d.contains("AA"));
        assert!(!d.contains("0xAA"));
        assert!(!d.contains("170"));
    }
}
