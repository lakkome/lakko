// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-shamir` (Phase 1.21b).
//!
//! Mirror of the Rust crate's public surface, flattened for
//! Swift / Kotlin idioms:
//!
//! - `Share` becomes a flat record of `(index: u8, data: Vec<u8>)`.
//!   No opaque handle; the underlying type already derives
//!   `Zeroize`+ `ZeroizeOnDrop` on the Rust side, but the
//!   `Vec<u8>` that crosses the UniFFI boundary loses that
//!   property — callers are responsible for clearing the bytes
//!   after combining. Same posture as `EncryptedAttachment` in
//!   1.19b: the convenience of flat records outweighs the
//!   zero-after-drop loss for the recovery flow's once-per-
//!   restore cadence.
//! - `split` / `combine` / `encode_share` / `decode_share` are
//!   top-level free functions, surfacing in Swift + Kotlin as
//!   free functions (matches the `composeContactRequest` +
//!   `powSolve` + `composeEnvelope` precedent).
//! - `ShamirError` collapses to a single `FfiError::Shamir(String)`
//!   variant — UI handling can branch on the static `kind` text
//!   if a future flow wants to distinguish "wrong number of
//!   shares" from "duplicate indices", but for the 1.21 social-
//!   recovery flow the user-visible message is uniformly
//!   "this combination of shares doesn't reconstruct the secret"
//!   regardless of which internal variant tripped.
//!
//! ## Why no `Vec<Share>`-returning ergonomics dance
//!
//! UniFFI happily round-trips `Vec<Record>` over the FFI on both
//! Swift and Kotlin — that's the canonical shape for both
//! generators. The wasm-bindgen mirror (1.21b wasm side) needs a
//! `SplitResult`/index-accessor builder because of the
//! `Vec<NonCopy>` limitation, but UniFFI is the easier half.

use lakko_shamir as inner;

use crate::FfiError;

// ============================================================================
// FfiError integration
// ============================================================================

impl From<inner::ShamirError> for FfiError {
    fn from(e: inner::ShamirError) -> Self {
        FfiError::Shamir(e.to_string())
    }
}

// ============================================================================
// Records
// ============================================================================

/// One share of a Shamir-split secret.
///
/// `index` is the polynomial evaluation point (`1..=n_shares`,
/// never zero — zero is the secret itself). `data` is the
/// per-byte polynomial values, same length as the original
/// secret.
///
/// **Caller-side zeroize**: the inner `lakko_shamir::Share`
/// derives `Zeroize` / `ZeroizeOnDrop`, but the `Vec<u8>` that
/// crosses the FFI does not. Swift / Kotlin callers should wipe
/// the `data` field after combining — `.fill(0)` (Swift) or
/// `.fill(0)` (Kotlin) is the canonical idiom.
#[derive(uniffi::Record, Clone)]
pub struct Share {
    /// Polynomial evaluation point. Non-zero by construction
    /// (zero is the secret itself, never an emitted share).
    pub index: u8,
    /// Polynomial values, one per secret byte. Same length as
    /// the original secret.
    pub data: Vec<u8>,
}

// ============================================================================
// Free functions
// ============================================================================

/// Split `secret` into `n_shares` shares such that any
/// `threshold` of them suffice to reconstruct it via
/// [`combine`]. `threshold` must be at least 2 and at most
/// `n_shares`; `n_shares` is capped at 16.
///
/// Each output `Share` carries a unique `index` in
/// `1..=n_shares`.
///
/// **Not deterministic** — fresh polynomial coefficients per
/// call, so the same `secret` produces different shares each
/// invocation. Recovery via [`combine`] IS deterministic.
///
/// # Errors
///
/// - [`FfiError::Shamir`] for every input-validation failure
///   (empty / oversize secret, threshold out of range, share
///   count out of range).
#[uniffi::export]
pub fn split(secret: Vec<u8>, threshold: u8, n_shares: u8) -> Result<Vec<Share>, FfiError> {
    let shares = inner::split(&secret, threshold, n_shares)?;
    // Each inner `Share` derives `ZeroizeOnDrop`, which forbids
    // moving fields out. Clone the `data` Vec into the FFI
    // record — the inner Share immediately drops and zeroes its
    // own copy; the clone we hand back is intentionally not
    // zeroize-on-drop (documented in the module doc above).
    Ok(shares
        .iter()
        .map(|s| Share {
            index: s.index,
            data: s.data.clone(),
        })
        .collect())
}

/// Reconstruct the secret from `shares` via Lagrange
/// interpolation at `x = 0` over GF(256). Returns the original
/// secret bytes when `shares.len()` is at least the original
/// split's `threshold`; returns garbage (a uniformly random
/// byte vector) when called with fewer than `threshold` shares.
///
/// The library deliberately does NOT store `threshold` on the
/// wire — the recovery UX is "collect K shares, hit Recover,
/// see whether the result decodes as a valid 24-word phrase".
/// A wrong K surfaces as a BIP39 checksum failure downstream,
/// not as a helpful error here.
///
/// # Errors
///
/// - [`FfiError::Shamir`] when fewer than 2 shares are
///   supplied, on duplicate share indices, on mismatched share
///   lengths.
#[uniffi::export]
pub fn combine(shares: Vec<Share>) -> Result<Vec<u8>, FfiError> {
    let inner_shares: Vec<inner::Share> = shares
        .into_iter()
        .map(|s| inner::Share {
            index: s.index,
            data: s.data,
        })
        .collect();
    inner::combine(&inner_shares).map_err(Into::into)
}

/// Encode a share to its human-typable wire form
/// `<index_decimal>:<base64>`. Mirror of
/// `lakko_shamir::Share::encode`.
#[uniffi::export]
pub fn encode_share(share: Share) -> String {
    inner::Share {
        index: share.index,
        data: share.data,
    }
    .encode()
}

/// Parse a share from the [`encode_share`] wire form.
///
/// # Errors
///
/// Returns [`FfiError::Shamir`] when the input does not match
/// the `<index>:<base64>` shape, the index is not a 1..=255
/// decimal, the base64 fails to decode, or the data is empty /
/// oversize.
#[uniffi::export]
pub fn decode_share(encoded: String) -> Result<Share, FfiError> {
    let s = inner::Share::decode(&encoded)?;
    // Same `ZeroizeOnDrop` workaround as `split` — clone the
    // data Vec because we can't move out of a Drop-implementing
    // struct.
    Ok(Share {
        index: s.index,
        data: s.data.clone(),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_then_combine_round_trip_through_ffi() {
        let secret = b"recovery seed via uniffi".to_vec();
        let shares = split(secret.clone(), 3, 5).unwrap();
        assert_eq!(shares.len(), 5);
        let recovered = combine(shares.into_iter().take(3).collect()).unwrap();
        assert_eq!(recovered, secret);
    }

    #[test]
    fn split_rejects_threshold_one_via_ffi_error() {
        // Use `matches!` rather than `unwrap_err()` so the test
        // doesn't require `Debug` on `Share` — leaking share
        // bytes through a Debug derive is the wrong default for
        // a secret-bearing struct.
        assert!(matches!(
            split(b"x".to_vec(), 1, 3),
            Err(FfiError::Shamir(_))
        ));
    }

    #[test]
    fn combine_rejects_single_share_via_ffi_error() {
        let shares = split(b"abc".to_vec(), 2, 3).unwrap();
        assert!(matches!(
            combine(shares.into_iter().take(1).collect()),
            Err(FfiError::Shamir(_)),
        ));
    }

    #[test]
    fn combine_rejects_duplicate_indices_via_ffi_error() {
        let shares = split(b"abc".to_vec(), 2, 3).unwrap();
        let first = shares[0].clone();
        assert!(matches!(
            combine(vec![first.clone(), first]),
            Err(FfiError::Shamir(_)),
        ));
    }

    #[test]
    fn encode_decode_round_trip_through_ffi() {
        let shares = split(b"hello".to_vec(), 2, 3).unwrap();
        for share in shares {
            let wire = encode_share(share.clone());
            let recovered = decode_share(wire).unwrap();
            assert_eq!(recovered.index, share.index);
            assert_eq!(recovered.data, share.data);
        }
    }

    #[test]
    fn decode_rejects_missing_colon_via_ffi_error() {
        assert!(matches!(
            decode_share("nodot".to_string()),
            Err(FfiError::Shamir(_))
        ));
    }

    #[test]
    fn decode_rejects_index_zero_via_ffi_error() {
        assert!(matches!(
            decode_share("0:AAAA".to_string()),
            Err(FfiError::Shamir(_)),
        ));
    }
}
