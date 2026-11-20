<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-ballot/src/proof.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if proof.rs changes and this file does not. -->

# `proof` — linkable one-out-of-many proof (spec)

**Module:** `crates/lakko-ballot/src/proof.rs`
**One line:** Prove, in size logarithmic in the ring, that the holder of a secret owns one key of a ring and that a given tag is that secret's inverse times a tag base, without saying which key.

## Why this module exists

An anonymous group-poll ballot (ADR-0092 section 3.6) must show that its voter is in the poll's electorate and must carry a tag that is the same for every ballot one voter casts in one poll, so members count one vote per person without learning who. The maintainer chose a proof whose size grows with the logarithm of the ring (no LSAG, no fallback). This module is that proof and nothing else; the ballot module decides what it is bound to.

## Construction

Triptych (Noether and Goodell, 2020) in its plain linkable form, without the commitment-to-zero part: the Groth-Kohlweiss (2015) one-out-of-many proof with the Bootle et al. (2015) bit commitment, plus the linking tag. Relation, for ring `M_0..M_{L-1}`, tag base `U`, tag `J`:

```text
knowledge of (l, r) with M_l = r G and r J = U
```

`G` is the Ed25519 base point; the group is the prime-order subgroup of Edwards25519. Base `n = 2`, `m = max(1, ceil(log2 L))` digits, the ring padded to `N = 2^m` by repeating its last key. The prover and verifier equations are written out in the module header and follow the paper term for term. The prover commits `A`, `B`, `C`, `D` over all `n m` digit entries (including `i = 0`, whose `a` is minus the sum of the others), `X_j`, `Y_j`; the single challenge `x` is the Fiat-Shamir hash of the transcript; the responses are `f_{j,1}` (the verifier rebuilds `f_{j,0} = x - f_{j,1}`), `z_A`, `z_C`, `z`.

The transcript (BLAKE3 derive-key, domain `lakko/poll/ballot-proof/v1`, each item length-framed) binds, in order: the base, `m`, the ring length, a BLAKE3 digest of the ring keys (domain `lakko/poll/ballot-proof/ring/v1`), the caller's context, `U`, `J`, `A`, `B`, `C`, `D`, every `X_j`, every `Y_j`. The challenge is 64 XOF bytes reduced modulo the group order.

## Deviations from the papers

- Generators `H`, `G_{j,i}` and the tag base come from a try-and-increment hash-to-point (`point`), not RFC 9380.
- Prover randomness is hedged: an XOF keyed by 32 fresh OS bytes, the secret and the statement, so a broken random source alone does not repeat nonces (a repeated nonce across two proofs with different challenges would reveal `r` through `z`).
- Four separate multiscalar-multiplication checks; no random linear combination within a proof, and the papers' batch verification across proofs is not built.
- Padding repeats the last ring key instead of adding fresh points; the set of keys a prover may know a secret of is unchanged.

## Public contract

- `digits_for(len)`: `m` for a ring of `len` keys; `InvalidRing` for 0, `RingTooLarge` above `2^8` (the anonymity set).
- `capacity(m)`, `encoded_len(m)`: `2^m` and the proof size `1 + 32 (7 + 3m)` bytes.
- `Proof::encode` / `Proof::decode`: the wire layout below; decode checks the digit byte and the exact length only.
- `prove` (crate-private): refuses an index outside the ring (`NotInRing`) and a secret that does not match the key at the index (`WitnessMismatch`), so an honest prover never emits a proof that will not verify.
- `verify` (crate-private): `Ok(())` only if the proof's `m` matches the ring, every point decodes canonically into the prime-order subgroup, every scalar is canonical, and all four equations hold.

## Wire layout

```text
m(1) || A || B || C || D || X_0..X_{m-1} || Y_0..Y_{m-1} || f_{0,1}..f_{m-1,1} || z_A || z_C || z
```

Every point and scalar 32 bytes; `1 <= m <= 8`.

## Measured size and time

Since the 2026-11-15 rework a proof covers the anonymity set of the ballot (`ballot`), at most 256 keys, so `m <= 8` and the proof is at most 993 bytes whatever the group size.

Three single runs of `cargo run --release -p lakko-ballot --example measure` on 2026-11-15, Apple M3 Pro, 36 GiB, macOS, while other builds were running on the machine (load average about 38 to 44): timings are inflated by an unknown factor and are given as the range over the three runs; no phone has been measured. Sizes are exact. "Ring" is the size of the ring version the voter draws from; the set is `min(256, ring)`.

| Ring | m | Proof bytes | Row bytes | Ring build ms | Cast ms | Open + verify ms |
|---|---|---|---|---|---|---|
| 16 | 4 | 609 | 830 | 0.0 | 3.8 to 7.2 | 3.5 to 3.9 |
| 256 | 8 | 993 | 2,366 | 0.0 | 72 to 97 | 30 to 32 |
| 4,096 | 8 | 993 | 2,366 | 0.3 to 0.7 | 93 to 112 | 24 to 31 |
| 65,536 | 8 | 993 | 2,366 | 9.8 to 14 | 104 to 110 | 28 to 30 |
| 262,144 | 8 | 993 | 2,366 | 42 to 76 | 85 to 140 | 20 to 33 |

"Cast" is the draw, decoding the 256 selected keys, proving with constant-time multiscalar multiplications, and sealing. "Ring build" is `Ring::new` (it no longer decodes keys as points; a set's keys are decoded when it is used). The row is 2,366 bytes for every ring above 256 keys, of which 1,026 bytes are the set's index list.

Before the rework, proving over the whole ring took, in the same kind of loaded single runs, 1.3 to 2.3 s at 4,096 keys and 28 to 42 s at 65,536.

## Invariants upheld

- No proof verifies for a ring, context or tag other than the ones it was made for (all are in the transcript).
- No point with a small-order component and no non-canonical encoding enters an equation.
- The prover never reads its secret in variable-time code, as far as curve25519-dalek's constant-time multiscalar multiplication and scalar arithmetic are constant time; loop bounds depend only on public sizes. The digit extraction uses integer division on the index (the compiler may not make that constant time).
- Secret intermediates (`r_A..r_D`, `a`, `sigma`, `rho`, the polynomial coefficients, the nonce seed) are zeroized after use.

## Failure modes

Every error is a `BallotError`; nothing panics on any input. `ProofRejected` covers every verification failure (wrong length for the ring, bad point, bad scalar, failed equation) so the reason is not an oracle.

## What this module does NOT do

- It does not choose the context, the tag base or the ring version; `ballot` does.
- It does not check that a ring has no duplicate keys; `ring` does.
- No batch verification, no proof aggregation.

## Attacker exposure

`Proof::decode` and `verify` take bytes any group member or the Delivery Service can post. Bounded by the exact length check before any allocation proportional to `m`. Fuzz target `lakko-ballot/ballot_open` reaches both, asserting that nothing verifies against a ring of hash-to-point keys nobody holds. Not run in this session (no `cargo-fuzz` installed); compile-checked.

## References

- ADR-0092 sections 3.4, 3.6, 3.7, 10; ADR-0046 (G-39).
- Groth and Kohlweiss, EUROCRYPT 2015; Bootle et al., ESORICS 2015; Noether and Goodell, "Triptych", 2020.
- Tests: `proof::tests::completeness_at_every_position_for_small_rings`, `a_tag_of_another_secret_does_not_verify`; `tests/properties.rs` `every_tampered_proof_byte_is_refused`, `a_non_member_cannot_produce_a_verifying_ballot`.
