<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/proof.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if proof.rs changes and this file does not. -->

# `proof` — proof of knowledge of a linear relation (spec)

**Module:** `crates/lakko-anon-cred/src/proof.rs`
**One line:** Prove and verify knowledge of scalars `w_i` with `P_j = sum w_i * B_jk` for public points, non-interactively, in the compact `challenge || responses` form.

## Why this module exists

Every relation the credential needs (knowledge of `s` behind `C`, the issuer's use of its key, a valid MAC behind a presentation with the same `s` as the tag) is a set of linear equations over secret scalars. One generic prover and verifier, the generalised Schnorr protocol CMZ14 and Signal's `poksho` use, written once, is easier to review than three hand-rolled proofs. ADR-0090 4.7 also names this proof layer as reusable by ADR-0086.

## Public contract (crate-internal)

- `Statement { witness_count, equations }`, `Equation { lhs, terms: [(witness index, base)] }`.
- `Proof::prove(transcript, statement, witness)` — refuses with `Internal` a witness of the wrong length, a term index that names no witness, or a witness that does not satisfy every equation: no proof of a false statement is produced. Appends the statement (witness count, equation count, each left-hand side, each term's index and base), draws one nonce per witness, appends the commitments `A_j = sum k_i * B_jk`, takes `c` and returns `r_i = k_i - c * w_i`. Prover arithmetic on secrets uses constant-time scalar multiplication.
- `Proof::verify(transcript, statement)` — false on a response-count mismatch or an out-of-range index; recomputes `A_j' = sum r_i * B_jk + c * P_j` (variable time: every input is public), appends the statement and the `A_j'` the same way and compares challenges in constant time.
- `to_bytes` / `decode(bytes, witness_count)` — `c || r_0 .. r_{n-1}`, each 32 bytes little-endian; `decode` requires exactly `32 * (n + 1)` bytes and canonical scalars.

## Nonces

`k_i = wide-reduce(BLAKE3-derive-key("lakko/anon-cred/v1 proof-nonce", state_digest || 32 OS-random bytes || i_be || w_i))`. Fresh randomness makes every proof different; mixing in the transcript state and the witness means a repeated or weak RNG output alone does not repeat a nonce across different statements or witnesses. An RNG failure is `RandomnessUnavailable`, never a fixed nonce.

## Invariants upheld

- Special soundness and honest-verifier zero knowledge of the Schnorr protocol for linear relations, made non-interactive by Fiat-Shamir in the random-oracle model.
- The statement is bound in the challenge, so a proof does not transfer to another statement of the same shape.

## Failure modes

`Internal` (prover given a false statement), `RandomnessUnavailable`, `Malformed` / `NonCanonical` on decode. `verify` returns a bool and never panics.

## Attacker exposure

`decode` and `verify` run on peer bytes. Lengths are fixed by the caller's witness count before any allocation.

## References

- CMZ14 section 4 and Camenisch-Stadler notation; Signal `poksho` (reference only, no code taken).
- Tests: `completeness_and_soundness_checks`, `false_statement_is_refused_by_prover`, `bad_index_is_refused`.
