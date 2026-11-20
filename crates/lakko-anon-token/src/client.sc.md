<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-token/src/client.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if client.rs changes and this file does not. -->

# `client` — blind and finalize anonymous tokens (spec)

**Module:** `crates/lakko-anon-token/src/client.rs`
**One line:** Pick a nonce per token, blind its input for the issuer, and turn the issuer's evaluations into tokens only after its proof verifies.

## Why this module exists

The issuer must evaluate a token without seeing it, or it could link the account that drew a token to the service where it was spent. Blinding hides the input; the DLEQ proof stops the issuer undoing that by evaluating one account under a key of its own and recognising the output later.

## Public contract

- `TokenRequest::new(context, epoch, count)` — `count` in `1..=MAX_BATCH` else `BadBatchSize`. For each token: a fresh 32-byte nonce from the OS CSPRNG, the input `token_input(context, epoch, nonce)`, and a VOPRF-mode blind. The request holds the blinding factors and is as sensitive as the tokens it becomes; its `Debug` prints context, epoch and count only.
- `single_with(context, epoch, nonce, blind_rng)` (crate-private, ADR-0097) — a one-token request whose nonce the caller gives and whose blinding factor is drawn from the caller's RNG. Used only by `SupporterCode::token_request`, which feeds a deterministic BLAKE3-XOF stream keyed by the code and the epoch, so the same `(code, epoch)` always yields the same blinded element (a retry of a lost redeem response repeats the request instead of asking for a second evaluation). The RFC 9497 blind is unchanged: 64 bytes reduced modulo the group order, a zero scalar skipped.
- `blinded_elements()` — what to send to `POST /auth/tokens`, in order; `blinded_elements_b64url()` the same as base64url without padding, the text the issuance body's `blinded_elements` field carries.
- `finalize_b64url(self, evaluations, proof, public_key)` — `finalize` over the base64url text the issuer and the key route send. Each field is length-checked on the text and decoded into its fixed width before `finalize` runs; a field that does not decode maps to the error its binary form would raise (an evaluation to `BadElement`, the proof to `ProofVerification`, the key to `BadPublicKey`). Added in ADR-0054 S3 so both FFI surfaces share one decoder.
- `finalize(self, evaluations, proof, public_key)` — consumes the request. Refuses: a count mismatch (`CountMismatch`); an invalid or identity public key (`BadPublicKey`); an invalid or identity evaluation (`BadElement`); a malformed proof, or one that does not verify for these blinded elements, evaluations and `public_key` (`ProofVerification`). All or nothing: no token is returned unless the whole batch verifies.

## Invariants upheld

- The caller must pass the public key obtained from the published key route (`GET /auth/tokens/key/{context}/{epoch}`), never the one echoed in the issuance response, or the tagging check proves nothing. This module cannot enforce where the key came from; it verifies against whatever it is given.
- Evaluations are bound to their order: swapping two, or supplying another request's evaluations and proof, fails the proof.

## What this module does NOT do

- No network, no persistence, no pool management (the clients' job in S3).
- It does not serialize a pending request; one that is dropped before `finalize` is lost, and the quota it was charged is not refunded.

## References

- ADR-0054 decision 2; ADR-0097 section 1 (supporter codes); RFC 9497 (VOPRF mode, batched proofs).
- Tests (`tests/round_trip.rs`): `an_evaluation_under_another_key_is_refused_by_the_client`, `a_tampered_proof_or_evaluation_is_refused_by_the_client`, `the_client_refuses_bad_shapes`, `a_batch_round_trips_and_every_token_verifies_once_per_spend_id`, `the_base64url_exchange_round_trips_and_the_header_value_verifies`, `the_base64url_finalize_refuses_bad_text`.
