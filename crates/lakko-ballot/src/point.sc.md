<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-ballot/src/point.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if point.rs changes and this file does not. -->

# `point` — group elements, hash-to-point, the voter key (spec)

**Module:** `crates/lakko-ballot/src/point.rs`
**One line:** Decode points and scalars strictly, derive generators nobody knows a discrete logarithm of, and turn an Ed25519 seed into the proof's witness.

## Public contract

- Point decoding (crate-private): a 32-byte string is accepted only if it decompresses, re-encodes to the same bytes (refusing a y-coordinate at or above p) and is torsion-free (`is_torsion_free`). Ring keys and tags additionally refuse the identity.
- Scalar decoding: canonical only (`Scalar::from_canonical_bytes`).
- `hash_to_point(domain, input)`: BLAKE3 derive-key over `input || counter` for counter 0..=255; the first output that decompresses as an Edwards y-coordinate is multiplied by the cofactor; the identity is skipped. Deterministic, variable time (public inputs). `HashToPoint` if no counter works (probability about 2^-256).
- Commitment generators: `H = hash_to_point("lakko/poll/commit-gen/v1", "H")`, `G_i = hash_to_point(same domain, "G" || i u32 BE)`.
- `VoterKey::from_ed25519_seed(seed)`: the scalar is the clamped, reduced SHA-512 expansion of the seed (ed25519-dalek's `to_scalar`), the public key the Ed25519 verifying key of that seed; zeroized on drop; `Debug` prints the public key only.

## Deviation, for the review

`hash_to_point` is try-and-increment, not RFC 9380: outputs are not uniform over the subgroup. The proof needs only that no discrete-logarithm relation between these points and `G` is known. curve25519-dalek 4.1.3 has no RFC 9380 Edwards map (its `nonspec_map_to_curve` is deprecated as not a secure hash), and Ristretto cannot be used because Ed25519 keys are Edwards points with no public conversion.

## The witness question (ADR-0090 open question 1)

The voter proves with the Ed25519 signing scalar of the key its leaf is bound to, the maintainer's recommendation. The same scalar then signs Ed25519 messages and proves ring membership; the transcripts are domain-separated and a proof is not an EdDSA signature, but no proof of joint security is cited. Listed for G-39.

## References

ADR-0090 section 3.6 ("The witness", "Key checks"). Tests: `point::tests::*`.
