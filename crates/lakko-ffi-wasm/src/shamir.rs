// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-shamir` (Phase 1.21b).
//!
//! Mirror of the UniFFI shape from `crates/lakko-ffi-uniffi/src/shamir.rs`,
//! adapted for the wasm-bindgen idiom:
//!
//! - `Share` is a `#[wasm_bindgen]` handle class with `(index,
//!   data)` getters + a constructor. Same `Vec<NonCopy>`
//!   workaround as 1.19b: wasm-bindgen does not round-trip a
//!   `Vec<Share>` directly, so [`split`] returns a
//!   [`SplitResult`] handle with `count` + `at(i) -> Share`
//!   accessors, and [`combine`] takes a [`ShareCollector`]
//!   builder (`new()` → `add(share)*` → `combine()`).
//! - `encodeShare(share) -> string` and
//!   `decodeShare(string) -> Share` are top-level free
//!   functions, JS-camelCase via `#[wasm_bindgen(js_name = ...)]`.
//! - Errors go through `JsError` with the inner
//!   `lakko_shamir::ShamirError`'s `Display` preserved — same
//!   posture as the rest of the wasm surface.
//!
//! ## Why a builder for `combine`
//!
//! wasm-bindgen-generated `.d.ts` for a function taking
//! `Vec<Share>` writes the parameter type as `(Share)[]`, but
//! the underlying ABI marshals each `Share` as a wasm pointer
//! to an opaque handle — wasm-bindgen has no built-in vec
//! marshaller for non-`Copy` handle types. The builder pattern
//! sidesteps the limitation cleanly: each `add(share)` is one
//! JS↔WASM call that hands a single pointer over, then
//! `combine()` consumes the accumulated `Vec<inner::Share>`
//! on the Rust side. Same shape `EnvelopeBuilder` uses in
//! `attachments.rs`.

use lakko_shamir as inner;
use wasm_bindgen::prelude::*;

/// Helper for turning a `ShamirError` into a `JsError` while
/// keeping the inner `Display` text on the JS side.
fn shamir_err(e: inner::ShamirError) -> JsError {
    JsError::new(&e.to_string())
}

/// Helper for malformed-input failures detected at the FFI
/// boundary (mirror of the `boundary_err` shape in
/// `attachments.rs`). Returns a `JsError` carrying the supplied
/// message verbatim.
fn boundary_err(msg: impl Into<String>) -> JsError {
    JsError::new(&msg.into())
}

// ============================================================================
// Share — wasm-bindgen handle class
// ============================================================================

/// One share of a Shamir-split secret. The `data` field is the
/// per-byte polynomial values; `index` is the polynomial
/// evaluation point (`1..=n_shares`, never zero).
///
/// **Caller-side zeroize**: the inner `lakko_shamir::Share`
/// derives `Zeroize` / `ZeroizeOnDrop`, but the wasm-bindgen
/// handle's `Vec<u8>` field does not. JS callers should drop
/// the reference + let the WASM allocator reclaim the memory
/// promptly after combining.
#[wasm_bindgen]
pub struct Share {
    index: u8,
    data: Vec<u8>,
}

#[wasm_bindgen]
impl Share {
    /// Construct from raw byte fields. Field-length validation
    /// happens at [`ShareCollector::add`] time — keeping the
    /// constructor permissive lets the [`decode_share`] +
    /// `at(i)` paths emit handles without re-validating.
    #[wasm_bindgen(constructor)]
    pub fn new(index: u8, data: Vec<u8>) -> Self {
        Self { index, data }
    }

    /// Polynomial evaluation point (1..=255).
    #[wasm_bindgen(getter)]
    pub fn index(&self) -> u8 {
        self.index
    }

    /// Polynomial values, one per secret byte.
    #[wasm_bindgen(getter)]
    pub fn data(&self) -> Vec<u8> {
        self.data.clone()
    }
}

// ============================================================================
// SplitResult — output of split (workaround for Vec<NonCopy>)
// ============================================================================

/// Output of [`split`]. Holds the share collection on the WASM
/// heap; JS iterates via `for (let i = 0; i < result.count;
/// i++) result.at(i)` to pull each [`Share`] handle out.
#[wasm_bindgen]
pub struct SplitResult {
    shares: Vec<inner::Share>,
}

#[wasm_bindgen]
impl SplitResult {
    /// Number of shares — exposed as a getter so JS can size
    /// arrays / iterators before pulling each share.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        u32::try_from(self.shares.len()).unwrap_or(u32::MAX)
    }

    /// Pull one share by index (`0..count`). wasm-bindgen does
    /// not export `Vec<Share>` over the boundary directly, so
    /// JS iterates `for (let i = 0; i < r.count; i++) r.at(i)`.
    ///
    /// # Errors
    ///
    /// `RangeError` on out-of-bounds `i`.
    #[wasm_bindgen]
    pub fn at(&self, i: u32) -> Result<Share, JsError> {
        let idx = usize::try_from(i).map_err(|_| boundary_err(format!("bad index {i}")))?;
        let s = self
            .shares
            .get(idx)
            .ok_or_else(|| boundary_err(format!("share index {i} out of bounds")))?;
        Ok(Share {
            index: s.index,
            data: s.data.clone(),
        })
    }
}

// ============================================================================
// ShareCollector — input builder for combine (workaround for Vec<NonCopy>)
// ============================================================================

/// Builder for [`combine`]. JS idiom:
///
/// ```ts
/// const collector = new ShareCollector();
/// for (const share of shares) collector.add(share);
/// const secret = collector.combine();
/// ```
#[wasm_bindgen]
pub struct ShareCollector {
    shares: Vec<inner::Share>,
}

#[wasm_bindgen]
impl ShareCollector {
    /// Start a fresh empty collector.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self { shares: Vec::new() }
    }

    /// Append a share. The handle's bytes are cloned into the
    /// builder's internal `inner::Share`; the JS-side handle
    /// stays usable for subsequent renders / UI access.
    #[wasm_bindgen]
    pub fn add(&mut self, share: &Share) {
        self.shares.push(inner::Share {
            index: share.index,
            data: share.data.clone(),
        });
    }

    /// Consume the builder + reconstruct the secret. See
    /// [`combine`] for the semantics + error variants.
    ///
    /// # Errors
    ///
    /// `JsError` wrapping the inner `ShamirError` on duplicate
    /// indices, mismatched share lengths, or fewer than 2
    /// shares accumulated.
    #[wasm_bindgen]
    pub fn combine(self) -> Result<Vec<u8>, JsError> {
        inner::combine(&self.shares).map_err(shamir_err)
    }
}

impl Default for ShareCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Free functions
// ============================================================================

/// Split `secret` into `n_shares` shares such that any
/// `threshold` of them suffice to reconstruct it via
/// [`ShareCollector::combine`]. Mirror of `lakko_shamir::split`.
///
/// Each output `Share` in the returned [`SplitResult`] carries
/// a unique `index` in `1..=n_shares`. Output is randomised —
/// repeated calls on the same `secret` produce different
/// shares each time (the polynomial coefficients are fresh
/// random per call); recovery is deterministic.
///
/// # Errors
///
/// `JsError` wrapping the inner `ShamirError` on empty /
/// oversize secret, out-of-range threshold, or out-of-range
/// share count.
#[wasm_bindgen]
pub fn split(secret: &[u8], threshold: u8, n_shares: u8) -> Result<SplitResult, JsError> {
    let shares = inner::split(secret, threshold, n_shares).map_err(shamir_err)?;
    Ok(SplitResult { shares })
}

/// Encode a share to its human-typable wire form
/// `<index_decimal>:<base64>`. Mirror of
/// `lakko_shamir::Share::encode`.
#[wasm_bindgen(js_name = encodeShare)]
#[must_use]
pub fn encode_share(share: &Share) -> String {
    inner::Share {
        index: share.index,
        data: share.data.clone(),
    }
    .encode()
}

/// Parse a share from the [`encode_share`] wire form. Mirror
/// of `lakko_shamir::Share::decode`.
///
/// # Errors
///
/// `JsError` wrapping the inner `ShamirError` when the input
/// doesn't match the `<index>:<base64>` shape, the index isn't
/// a 1..=255 decimal, the base64 fails to decode, or the data
/// is empty / oversize.
#[wasm_bindgen(js_name = decodeShare)]
pub fn decode_share(encoded: &str) -> Result<Share, JsError> {
    let s = inner::Share::decode(encoded).map_err(shamir_err)?;
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
    fn split_then_combine_round_trip() {
        let secret = b"recovery seed via wasm".to_vec();
        let result = split(&secret, 3, 5).unwrap();
        assert_eq!(result.count(), 5);
        let mut collector = ShareCollector::new();
        // Pick any 3 of the 5.
        collector.add(&result.at(0).unwrap());
        collector.add(&result.at(2).unwrap());
        collector.add(&result.at(4).unwrap());
        let recovered = collector.combine().unwrap();
        assert_eq!(recovered, secret);
    }

    #[test]
    fn encode_decode_round_trip() {
        let result = split(b"hi", 2, 3).unwrap();
        let s = result.at(1).unwrap();
        let wire = encode_share(&s);
        let recovered = decode_share(&wire).unwrap();
        assert_eq!(recovered.index(), s.index());
        assert_eq!(recovered.data(), s.data());
    }

    // Note: error-path tests for `split` / `SplitResult::at` /
    // `decode_share` are deliberately NOT here. The wasm
    // `JsError::new(...)` constructor panics on a non-wasm
    // target (host `cargo test`), which would cause the
    // assertions to abort before they reach the comparison.
    // Error coverage stays on the inner `lakko-shamir` crate +
    // the future `wasm-pack test --node` smoke suite (mirror
    // of the 1.19b attachments policy in this crate).
}
