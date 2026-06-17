// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call safety number — a short, human-comparable code derived
//! deterministically from the shared per-call seed.
//!
//! # What it is for
//!
//! Every participant in a call derives the **same** 32-byte seed: the
//! MLS exporter secret `group.export_secret("lakko/call/v1", call_id,
//! 32)`, identical for everyone in the same group + epoch (audit-pack
//! C-1). The safety number is a deterministic, domain-separated hash of
//! that seed, rendered as a short decimal code. Two participants read it
//! aloud over an out-of-band channel; if the codes match, both sides
//! share the same E2EE root key. A mismatch reveals a different key on
//! one side — a relayed or injected call — which is exactly the
//! man-in-the-middle case strict E2EE (security invariant #4) must let
//! users detect.
//!
//! # Why a separate derivation (not the raw seed)
//!
//! The seed itself is secret call-key material and must never be shown.
//! The safety number is a one-way [`blake3_derive_key_from_material`]
//! over the seed under a dedicated domain context, so displaying it
//! leaks nothing about the seed (a fresh domain means it cannot be
//! cross-used as any other key) while staying a stable fingerprint of
//! it.
//!
//! # Canonical format
//!
//! [`SAFETY_NUMBER_GROUPS`] groups of four decimal digits, single-space
//! separated (e.g. `"0123 4567 8901 2345"`). The format is fixed so web
//! / iOS / Android render byte-identical strings for the same seed — the
//! cross-platform interop the golden-vector test pins. Sixteen digits
//! (~53 bits) is comfortably beyond what an active man-in-the-middle
//! could second-preimage within a call-setup window, while staying short
//! enough for an in-call status pill and an aloud read.

use lakko_crypto::hash::blake3_derive_key_from_material;

/// Domain-separation context for the safety-number derivation. A
/// dedicated context means the derived material can never collide with
/// any other key derived from the same seed.
const DOMAIN: &str = "lakko/call/safety-number/v1";

/// Number of four-digit groups in a rendered safety number. Four groups
/// = 16 decimal digits, drawn from 16 of the 32 derived bytes.
pub const SAFETY_NUMBER_GROUPS: usize = 4;

/// Derive the human-comparable call safety number from the shared
/// 32-byte per-call seed.
///
/// The result is [`SAFETY_NUMBER_GROUPS`] groups of four decimal digits,
/// single-space separated. Deterministic and canonical: the same seed
/// always yields the same string on every platform.
#[must_use]
pub fn call_safety_number(seed: &[u8; 32]) -> String {
    let material = blake3_derive_key_from_material(DOMAIN, seed);
    // Each group consumes four big-endian bytes reduced into a 0..=9_999
    // bucket. `chunks_exact(4)` over the 32-byte material yields eight
    // chunks; we take the first SAFETY_NUMBER_GROUPS.
    material
        .chunks_exact(4)
        .take(SAFETY_NUMBER_GROUPS)
        .map(|c| {
            let value = u32::from_be_bytes([c[0], c[1], c[2], c[3]]) % 10_000;
            format!("{value:04}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic() {
        let seed = [0x42u8; 32];
        assert_eq!(call_safety_number(&seed), call_safety_number(&seed));
    }

    #[test]
    fn has_the_canonical_shape() {
        let s = call_safety_number(&[0x42u8; 32]);
        let groups: Vec<&str> = s.split(' ').collect();
        assert_eq!(groups.len(), SAFETY_NUMBER_GROUPS);
        for g in groups {
            assert_eq!(g.len(), 4, "group {g} is not four digits");
            assert!(
                g.chars().all(|c| c.is_ascii_digit()),
                "group {g} not all digits"
            );
        }
        // 4 groups * 4 digits + 3 separating spaces.
        assert_eq!(
            s.len(),
            SAFETY_NUMBER_GROUPS * 4 + (SAFETY_NUMBER_GROUPS - 1)
        );
    }

    #[test]
    fn distinct_seeds_yield_distinct_numbers() {
        let a = call_safety_number(&[0x00u8; 32]);
        let b = call_safety_number(&[0xFFu8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn one_bit_flip_changes_the_number() {
        let mut seed = [0u8; 32];
        let base = call_safety_number(&seed);
        seed[31] ^= 0x01;
        assert_ne!(base, call_safety_number(&seed));
    }

    #[test]
    fn golden_vector_is_stable() {
        // Pins the canonical derivation so a regression in the hash,
        // domain context, byte layout, or grouping is caught, and so the
        // web / iOS / Android renderers can be checked against the same
        // fixed vector. Computed firsthand from the all-zero seed.
        assert_eq!(call_safety_number(&[0u8; 32]), GOLDEN_ALL_ZERO);
    }

    const GOLDEN_ALL_ZERO: &str = "6397 5175 0657 3748";
}
