<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/group.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if group.rs changes and this file does not. -->

# `group` — Ristretto255 generators, hash-to-group and decoding (spec)

**Module:** `crates/lakko-anon-cred/src/group.rs`
**One line:** The two fixed generators `G` and `H`, the crate's hash-to-group, canonical point and scalar decoding, and fresh random scalars.

## Why this module exists

The credential's soundness rests on nobody knowing a discrete-log relation between `G` and `H` (and between either and a poll's tag base). Producing every base by hash-to-group on a fixed label gives that, and putting decoding in one place means every wire field is checked the same way.

## Public contract (crate-internal)

- `hash_to_group(label, input)` — `BLAKE3-derive-key("lakko/anon-cred/v1 hash-to-group")` in XOF mode over `label_len (u16 BE) || label || input`, 64 bytes, mapped with `RistrettoPoint::from_uniform_bytes` (the RFC 9496 section 4.3.4 one-way map). The length prefix keeps distinct `(label, input)` pairs from hashing the same stream.
- `generator_g()` (label `generator G (G_cred)`) and `generator_h()` (label `generator H`), computed once per process and cached. `G` is the ADR's `G_cred`: the base of the account commitment, of `U` at issuance and of the `U'` blinding; `H` is the base of the issuer's attribute keys and of every Pedersen blinding.
- `decode_point` — exactly 32 bytes (`Malformed` otherwise) that decompress as a canonical Ristretto255 encoding (`NonCanonical` otherwise). `decode_nonidentity_point` additionally refuses the identity (`IdentityPoint`).
- `decode_scalar` — exactly 32 bytes encoding a scalar below the group order (`NonCanonical` otherwise); no reduction of a non-canonical value.
- `random_nonzero_scalar` — 64 bytes from the OS RNG reduced modulo the order; zero is redrawn up to eight times; an RNG failure or eight zeros is `RandomnessUnavailable`. `random_bytes` — 32 OS-random bytes.

## Invariants upheld

- No generator is the identity, `G != H`, and neither is the Ristretto basepoint (unit test).
- Every decoded point is canonical; non-canonical encodings are refused, never normalised, so an encoding re-encodes to the same bytes.

## What this module does NOT do

- It does not decide which points may be the identity; callers choose `decode_point` or `decode_nonidentity_point` per field.

## Attacker exposure

Every point and scalar of every wire form passes through these decoders. They are fixed-length, allocation-free and panic-free.

## References

- ADR-0092 4.2 (`G_cred`), RFC 9496.
- Tests: `generators_are_distinct_and_not_identity`, `hash_to_group_separates_label_from_input`, `decoders_refuse_bad_input`.
