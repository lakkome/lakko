<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/transcript.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if transcript.rs changes and this file does not. -->

# `transcript` — Fiat-Shamir transcript (spec)

**Module:** `crates/lakko-anon-cred/src/transcript.rs`
**One line:** A labelled, length-prefixed BLAKE3 transcript that turns every public input of a proof into its challenge.

## Why this module exists

A non-interactive proof is only as binding as its challenge hash. Every public input (protocol, period, issuer key, poll, bound message, every presented point, the statement and the prover's commitments) must enter the hash unambiguously, so a proof made for one context is a proof of nothing in another.

## Public contract (crate-internal)

- `Transcript::new(protocol)` — BLAKE3 in derive-key mode with the context `lakko/anon-cred/v1 fiat-shamir`, then the labelled append `protocol`.
- `append(label, bytes)` — hashes `label_len (u16 BE) || label || len (u64 BE) || bytes`. `append_u64` appends 8 big-endian bytes; `append_point` the canonical compressed point.
- `challenge()` — on a copy of the state, the label `challenge`, then 64 XOF bytes reduced modulo the group order (`Scalar::from_bytes_mod_order_wide`). Calling it does not change the transcript.
- `state_digest()` — 32 bytes over the state so far, used only to hedge proof nonces.

## Invariants upheld

- Injective encoding: labels and lengths are explicit, so `("ab", "c")` and `("a", "bc")` give different challenges.
- Domain separation from every other BLAKE3 use in Lakko by the derive-key context, and between proof kinds by the protocol label.

## What this module does NOT do

- It does not decide what is bound; `relations` does.

## References

- Tests: `appends_are_unambiguous`, `protocol_label_separates`, `challenge_is_deterministic`.
