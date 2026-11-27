<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-contacts/src/reset_verdict.rs; keep in lock-step with the source (just spec-check). -->

# `reset_verdict` — ADR-0073 R.2 with rule 4 (spec)

**Module:** `crates/lakko-contacts/src/reset_verdict.rs`   **One line:** the
one decision a contact's device takes on an account-key reset: sealed (the
same person), unsealed (a new account under the same username), refused, or
no verdict yet.

## Why this module exists

Until 2026-11-26 the R.2 table was written three times, in the web, iOS and
Android clients. Rule 4 (the reset seal, ADR-0101 section 10) decides whether
a reset moves conversations, history, the profile and channel subscriptions,
so it is defined once here and exposed on both FFI surfaces. The function is
pure: the caller runs the checks that touch keys and proofs and hands in
their results.

## Public contract

`reset_verdict(&ResetVerdictInput) -> ResetVerdict`, in this order:

1. The fetched key equals the held key: `NoVerdict`.
2. Attestation absent or invalid: `Refused`. No attest key: `NoVerdict`.
3. Attested generation not equal to the bundle's, or not above the accepted
   one: `Refused`.
4. Key log: confirmed with `is_reset` true goes on; confirmed otherwise,
   mismatch or rollback is `Refused`; unreachable or absent is `NoVerdict`;
   off goes on when `LOG_OFF_ACCEPTS_RESET` (decision D3, `true`).
5. Rule 4: an invalid seal signature is `Unsealed`. A valid one is `Sealed`
   when the log is off, or when the seal's log half is `Proven`; `NotProven`
   is `Unsealed`; `Unreachable` is `NoVerdict`.

A refusal is `Unexplained` exactly when the log is confirmed with `is_reset`
false, `Unconfirmed` otherwise. `Sealed` carries the attested generation, its
mint time and the previous announcement's `seq` (the caller's new floor);
`Unsealed` carries the generation and mint time.

## Invariants upheld

- An unsealed reset never outranks a refusal: rules 1 to 3 are judged first.
- With the log on, a reset is sealed only when both halves of rule 4 hold.
- With the log off the seal rests on the held key's signature and the `seq`
  floor (ADR-0101 section 10's stated rollback residual).

## Failure modes

None: the function is total and never panics.

## What this module does NOT do

It verifies nothing itself: the attestation, the key log, the seal signature
(`lakko_attest::check_reset_seal`) and the seal's log half
(`lakko_transparency::verify_announcement_before_reset`) are the caller's.
It does not re-pin, move or keep anything.

## References

ADR-0073 R.2 and "Amended 2026-11-26 (second)", ADR-0101 sections 10 and 11.
Tests: `the_whole_table`, `an_unsealed_reset_never_outranks_a_refusal`.
