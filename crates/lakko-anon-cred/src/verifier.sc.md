<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/verifier.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if verifier.rs changes and this file does not. -->

# `verifier` — keyed verification of a presentation (spec)

**Module:** `crates/lakko-anon-cred/src/verifier.rs`
**One line:** The poll scope, the presentation wire form, and the channels service's check that a presentation comes from a valid credential and carries the right tag for this poll and request.

## Why this module exists

`services/channels` must refuse a second vote by one account in one poll while learning neither the account nor any link between one account's votes in two polls (ADR-0092 4.1, 4.3).

## Public contract

- `PollScope::channel_post(channel_id: [u8; 16], post_cursor: u64)` — the poll; `as_bytes()` = `channel_id || post_cursor_be` (24 bytes).
- `Presentation` — `version(1) || period(8 BE) || U~(32) || C_s(32) || C_U'(32) || T(32) || c(32) || 3 responses(96)` = 265 bytes. `decode` requires the exact length, version 1, canonical points with `U~` and `T` not the identity, canonical scalars. `tag()` is `T` compressed (32 bytes), unverified until `verify` accepts it.
- `IssuerSecretKey::verify_presentation(&presentation, &scope, message)` — `WrongPeriod` if the presentation is not for this key's period, `MessageTooLong`; computes `V = x0*U~ + x1*C_s + x2*p*U~ - C_U'` and checks the presentation proof bound to the period, the issuer key, the scope, the message and every point; `BadProof` otherwise. Returns `VerifiedPresentation { tag, period }`.
- `CredentialVerifier::new(master)` / `verify(bytes, &scope, message, current_period)` — `MessageTooLong`, structural decode, `PeriodNotAccepted` unless the period is the current or the previous one, then key derivation for the presented period and `verify_presentation`.

The caller compares the returned tag with the tag in its route and keeps one row per (poll, tag).

## Invariants upheld

- A presentation verifies for exactly one poll and one message: moving it to another post, another channel or another request content fails (`BadProof`).
- `U~ = identity` is refused at decode: with it the MAC check is vacuous and a forgery would be trivial.
- A presentation relabelled to another period fails (the period is in the transcript and in `V`).
- Nothing about the account reaches the verifier: `U~` is a fresh random multiple of `U`, `C_s` and `C_U'` are Pedersen commitments with fresh blinding, and `T` reveals only the poll-scoped tag.

## Failure modes

`Malformed`, `UnknownVersion`, `NonCanonical`, `IdentityPoint`, `PeriodNotAccepted`, `WrongPeriod`, `MessageTooLong`, `BadProof`, `ZeroScalar` (key derivation, negligible). Panic-free on any input.

## Measured (release build, Apple M3 Pro, this machine under a load average of about 30 from other builds; median of 200 runs per operation, range over five runs; the spread is the load, not a confidence interval)

- verify including key derivation: 671-1020 us; with the key already derived: 373-675 us; key derivation alone: 123-314 us. `examples/measure.rs` reproduces them.

## Attacker exposure

Every vote request's presentation is attacker-controlled and reaches `verify` before any state is consulted. Fixed length, bounded work (a constant number of scalar multiplications). Fuzz target `presentation_verify` asserts nothing the fuzzer produces verifies under a fixed key.

## References

- ADR-0092 4.3, 4.4; CMZ14 section 4.2 (Show).
- Tests: `presentation_verifies_only_for_its_post_and_message`, `period_window_is_own_period_and_the_next`, `a_presentation_relabelled_to_another_period_is_refused`, `a_credential_under_another_master_does_not_verify`, `identity_u_is_refused_at_decode`, `any_tampered_byte_is_refused`.
