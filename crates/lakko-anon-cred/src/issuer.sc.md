<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/issuer.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if issuer.rs changes and this file does not. -->

# `issuer` — auth's side of the credential (spec)

**Module:** `crates/lakko-anon-cred/src/issuer.rs`
**One line:** Accept a commitment registration, and answer an issuance request with a MAC on `(s, period)` computed from the stored commitment and a proof that the published key was used.

## Why this module exists

Auth issues one credential per account per period without learning `s`, and must not be able to tag an account by issuing under a key of its own.

## Public contract

- `verify_registration(&registration)` — returns the commitment to store when the proof of knowledge of `s` verifies, else `BadProof`. A commitment with an unknown discrete log, or another account's commitment, is refused.
- `IssuerSecretKey::issue(&request, &stored)` — `WrongPeriod` if the request is not for this key's period; `BadProof` if its proof does not show knowledge of the `s` behind `stored` bound to this period's public key (so a request made with another `s`, or for another key, is refused). Then draws a fresh non-zero `b`, sets `U = b*G`, `S = b*C` (equals `s*U`), `U' = x0*U + x1*S + x2*p*U`, and proves the issuance relation. The quota (one per account and period, ADR-0092 4.2) is the service's, not this function's.
- `IssuanceResponse` — `version(1) || period(8 BE) || U(32) || U'(32) || c(32) || 4 responses(128)` = 233 bytes; `decode` requires the exact length, version 1, canonical non-identity `U` and `U'`, canonical scalars; it does not verify.

## Deviation from the ADR text

ADR-0092 4.2 describes the request as an ElGamal encryption of `s` with a proof. In CMZ14 blind issuance that ciphertext encrypts `s*G` so the issuer can compute the MAC without seeing it; auth already stores `C = s*G`, so the ciphertext would hide nothing. The request is the proof of knowledge alone, and the issuer computes the same MAC in the clear. Auth still never sees `s`, and a presentation is unlinkable to the issuance because it is re-randomised.

## Measured (release build, Apple M3 Pro, this machine under a load average of about 30 from other builds; median of 200 runs per operation, range over five runs; the spread is the load, not a confidence interval)

- verify registration: 47-114 us; issue: 692-1592 us (includes verifying the request).

## Attacker exposure

The request and registration come from a signed-in client; their decoders are fixed-length and their proofs cannot be satisfied without `s`. The fuzz target `issuance_request_decode` asserts that no registration or request it builds verifies.

## References

- ADR-0092 4.2; CMZ14 section 4.2.
- Tests: `request_for_another_secret_than_the_stored_commitment_is_refused`, `a_request_bound_to_another_key_is_refused`, `registration_without_knowledge_is_refused`, `client_refuses_a_response_under_another_key_or_for_another_secret`.
