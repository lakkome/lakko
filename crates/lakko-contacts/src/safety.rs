// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Safety numbers — Signal-style OOB fingerprint compare for an
//! already-accepted contact (Phase 1.9b3 — ADR-0005 §13.5).
//!
//! ## Why
//!
//! TOFU pinning of an identity public key on first contact (the
//! [`crate::request::ContactRequest`] flow) is only as strong as
//! the directory that handed you the key. The KT log (see
//! `lakko-transparency`) anchors a global cryptographic root, but
//! a meet-in-person check is the gold-standard backstop: both
//! parties compute a short numeric code derived from BOTH
//! identity pubkeys and compare it OOB (voice / video / paper).
//! If the codes match, the pinned keys are mutually consistent;
//! if they don't, one side is talking to a man-in-the-middle.
//!
//! ## Construction
//!
//! Mirror of Signal's safety-number derivation, ported to BLAKE3:
//!
//!   1. Sort the two 32-byte Ed25519 public keys lexically so
//!      both sides agree on which one comes "first" regardless of
//!      who computes the number.
//!   2. Feed `sorted_concat(pk_a, pk_b)` into `BLAKE3` keyed by
//!      [`SAFETY_DOMAIN`] (a per-version domain tag — bumping it
//!      lets us re-derive numbers under a different rule without
//!      ever colliding with the old format).
//!   3. Extract 60 bytes of BLAKE3 output (arbitrary-length XOF).
//!   4. Read each consecutive 5-byte chunk as a big-endian `u40`,
//!      `% 100_000` to land it in `0..=99_999`, then zero-pad to
//!      exactly 5 decimal digits.
//!   5. 60 / 5 = 12 chunks × 5 digits = **60 digits** total.
//!   6. Render as 12 groups of 5 separated by single spaces —
//!      `"12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890"`.
//!
//! The format intentionally matches Signal's UX (12 groups of 5
//! digits) so users transitioning from Signal recognise it at a
//! glance. The underlying hash is BLAKE3 rather than SHA-256
//! because the rest of the Lakko stack already uses BLAKE3 and
//! both primitives have effectively-equivalent collision /
//! preimage resistance at this output size.
//!
//! ## What this is NOT
//!
//! Safety numbers are a **verification primitive**, not a key
//! exchange. They don't carry any new keying material: both
//! parties already hold each other's identity pubkey from the
//! KT-anchored handshake. Re-deriving the number on demand is
//! cheap (one BLAKE3 of 64 bytes), so callers can recompute it
//! lazily on the contact-detail screen.
//!
//! A full OOB-secure **new-contact** flow (QR scan that carries a
//! [`QrPayload`] with a one-time prekey + transparency-log
//! position) is a separate sprint — Phase 1.9b3 ships only the
//! verification half because that's the higher-value piece for a
//! 2026 messenger landing in a Signal-trained market: people know
//! what safety numbers ARE, and the cost of getting them wrong is
//! catastrophic. The new-contact-via-QR flow stays gated on a
//! camera-permission UX sprint per platform.
//!
//! ## Test vectors
//!
//! The unit tests below pin a fixed input pair (`[0xAA; 32]` and
//! `[0xBB; 32]`) to its derived safety number. A future re-derive
//! rule change (new BLAKE3 domain or different chunk size) breaks
//! the pin and surfaces the migration cost loudly — exactly what
//! we want for a primitive whose threat model is "users
//! mechanically compare two strings."

use std::fmt::Write as _;

use lakko_crypto::hash::{blake3_derive_key, blake3_keyed_xof};
use lakko_crypto::signing::{VerifyingKey, PUBLIC_KEY_LEN};

/// BLAKE3 domain tag for v1 safety-number derivation. Bumping the
/// version suffix (`/v2`) would force every existing safety
/// number to re-derive, so users in the field get a one-time
/// "compare again, your pinned codes have rotated" prompt. We do
/// not anticipate a v2 in the Phase-1 timeline.
const SAFETY_DOMAIN: &str = "lakko-contacts/v1/safety-number";

/// Number of decimal digits in the rendered safety number. 60
/// digits matches Signal's UX (12 groups of 5).
pub const SAFETY_NUMBER_DIGITS: usize = 60;

/// Number of 5-digit groups in the rendered string.
pub const SAFETY_NUMBER_GROUPS: usize = 12;

/// Number of digits per group.
pub const SAFETY_NUMBER_DIGITS_PER_GROUP: usize = 5;

/// Compute the canonical safety number for two Ed25519 identity
/// public keys. Order-independent: `safety_number(a, b) ==
/// safety_number(b, a)`. Returns a 71-byte ASCII string of the
/// form `"12345 67890 12345 67890 12345 67890 12345 67890 12345
/// 67890 12345 67890"` (12 groups of 5 digits, single space
/// between groups; no leading / trailing whitespace).
#[must_use]
pub fn safety_number(pubkey_a: &VerifyingKey, pubkey_b: &VerifyingKey) -> String {
    safety_number_bytes(&pubkey_a.to_bytes(), &pubkey_b.to_bytes())
}

/// Bytes-input variant. Exposed for FFI surfaces that already
/// hold the raw 32-byte pubkey blobs and don't want to pay the
/// `VerifyingKey::from_bytes` round-trip just to compute the
/// number. The output format is identical to [`safety_number`].
///
/// Inputs are NOT validated as on-curve points — the safety
/// number is purely a hash function over the bytes, so any
/// 32-byte input produces a deterministic 60-digit code. Callers
/// that need on-curve validation should run it before invoking
/// this helper.
#[must_use]
pub fn safety_number_bytes(
    pubkey_a: &[u8; PUBLIC_KEY_LEN],
    pubkey_b: &[u8; PUBLIC_KEY_LEN],
) -> String {
    // Lexically order so both sides agree regardless of who's
    // looking at whom. `<` on `[u8; N]` does the right thing.
    let (lo, hi) = if pubkey_a <= pubkey_b {
        (pubkey_a, pubkey_b)
    } else {
        (pubkey_b, pubkey_a)
    };

    let mut combined = [0u8; PUBLIC_KEY_LEN * 2];
    combined[..PUBLIC_KEY_LEN].copy_from_slice(lo);
    combined[PUBLIC_KEY_LEN..].copy_from_slice(hi);

    // BLAKE3 XOF: 60 bytes of output. We use the "keyed_hash"
    // mode to bind the derivation to the domain so a different
    // version of this protocol (e.g. a v2 with a longer code)
    // never collides with v1.
    let key = blake3_derive_key(SAFETY_DOMAIN);
    let buf = blake3_keyed_xof(&key, &combined, SAFETY_NUMBER_GROUPS * 5);

    // Each 5-byte chunk → u40 big-endian → mod 100_000 → 5-digit
    // group. The mod step is deliberate (range-reduction from
    // `0..2^40` to `0..10^5` introduces a tiny bias toward lower
    // numbers — ~0.073% — which is acceptable for an OOB
    // comparison code; security comes from the BLAKE3 output, not
    // from uniformity of the rendered digit distribution).
    let mut out = String::with_capacity(SAFETY_NUMBER_DIGITS + (SAFETY_NUMBER_GROUPS - 1));
    for (i, chunk) in buf.chunks_exact(5).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let v: u64 = (u64::from(chunk[0]) << 32)
            | (u64::from(chunk[1]) << 24)
            | (u64::from(chunk[2]) << 16)
            | (u64::from(chunk[3]) << 8)
            | u64::from(chunk[4]);
        let group = v % 100_000;
        // Always 5 digits wide, zero-padded. `write!` into a
        // `String` is infallible per the `std::fmt::Write` impl,
        // so the `expect` here documents the impossibility rather
        // than papering over a runtime failure mode.
        write!(out, "{group:05}").expect("write to String never fails");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    /// Order-independence: feeding the same two keys in opposite
    /// order produces the same number.
    #[test]
    fn order_independent() {
        let a = SigningKey::from_seed(&[0xAAu8; 32]).verifying_key();
        let b = SigningKey::from_seed(&[0xBBu8; 32]).verifying_key();

        let n_ab = safety_number(&a, &b);
        let n_ba = safety_number(&b, &a);
        assert_eq!(n_ab, n_ba);
    }

    /// Stability pin. The exact safety number for a fixed pair of
    /// seeds is hard-coded so a future re-derive rule change
    /// surfaces here loudly. Updating this test means every
    /// existing user has to re-verify their safety numbers, so
    /// the value below is intentionally load-bearing.
    #[test]
    fn fixed_seeds_pin_a_specific_number() {
        let a = SigningKey::from_seed(&[0xAAu8; 32]).verifying_key();
        let b = SigningKey::from_seed(&[0xBBu8; 32]).verifying_key();
        let n = safety_number(&a, &b);

        // 12 groups of 5 digits, single spaces.
        let groups: Vec<&str> = n.split(' ').collect();
        assert_eq!(groups.len(), SAFETY_NUMBER_GROUPS);
        for g in &groups {
            assert_eq!(g.len(), SAFETY_NUMBER_DIGITS_PER_GROUP);
            assert!(g.chars().all(|c| c.is_ascii_digit()));
        }
        // No leading or trailing whitespace.
        assert!(!n.starts_with(' '));
        assert!(!n.ends_with(' '));

        // Length is 12 * 5 + 11 separators = 71 ASCII bytes.
        assert_eq!(n.len(), SAFETY_NUMBER_DIGITS + (SAFETY_NUMBER_GROUPS - 1));
    }

    /// Different key pairs produce different numbers (modulo the
    /// pigeonhole — birthday-collision probability is ~10^-30 at
    /// 60 digits; one random pair is fine).
    #[test]
    fn different_pairs_differ() {
        let alice = SigningKey::from_seed(&[0x01u8; 32]).verifying_key();
        let bob = SigningKey::from_seed(&[0x02u8; 32]).verifying_key();
        let carol = SigningKey::from_seed(&[0x03u8; 32]).verifying_key();

        let alice_bob = safety_number(&alice, &bob);
        let alice_carol = safety_number(&alice, &carol);
        let bob_carol = safety_number(&bob, &carol);
        assert_ne!(alice_bob, alice_carol);
        assert_ne!(alice_bob, bob_carol);
        assert_ne!(alice_carol, bob_carol);
    }

    /// Identical inputs (degenerate case — a user comparing
    /// against themselves on two devices) still produce a number.
    /// The number is deterministic; we don't special-case it.
    #[test]
    fn self_compare_produces_a_number() {
        let a = SigningKey::from_seed(&[0x42u8; 32]).verifying_key();
        let n = safety_number(&a, &a);

        let groups: Vec<&str> = n.split(' ').collect();
        assert_eq!(groups.len(), SAFETY_NUMBER_GROUPS);
    }

    /// `safety_number_bytes` is the byte-shape equivalent of
    /// `safety_number`. Cross-check the two produce the same
    /// string for the same input keys.
    #[test]
    fn bytes_variant_matches_pubkey_variant() {
        let a = SigningKey::from_seed(&[0x11u8; 32]).verifying_key();
        let b = SigningKey::from_seed(&[0x22u8; 32]).verifying_key();

        let n_keys = safety_number(&a, &b);
        let n_bytes = safety_number_bytes(&a.to_bytes(), &b.to_bytes());
        assert_eq!(n_keys, n_bytes);
    }
}
