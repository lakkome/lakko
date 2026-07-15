<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/safety_number.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if safety_number.rs changes and this file does not. -->

# `safety_number` — call safety-number derivation (spec)

**Module:** `crates/lakko-calls/src/safety_number.rs`
**One line:** Turn the shared per-call seed into a short, human-comparable decimal code so two participants can detect a man-in-the-middle out of band.

## Why this module exists

Everyone in the same MLS group at the same epoch derives the identical 32-byte call seed (`group.export_secret("lakko/call/v1", call_id, 32)`, C-1). This module renders that seed as a short decimal fingerprint that two participants read aloud over an independent channel (a known-voice line, an in-person check). Matching codes prove both sides hold the same E2EE root key; a mismatch means one side has a different key — a relayed or injected call. Strict E2EE (security invariant #4) forbids a silent non-E2EE fallback, and this code is the user-facing tool that lets a human catch the case where a MITM has substituted keys rather than downgraded.

## Public contract

- `call_safety_number(seed: &[u8; 32]) -> String` — the sole entry point. Deterministic and canonical: the same seed always yields the same string, byte-for-byte, on every platform. `#[must_use]`, infallible (no `Result`), never panics — the fixed-size array input and `chunks_exact(4)` over the 32-byte hash make every index provably in bounds.
- `SAFETY_NUMBER_GROUPS` (= 4) — the number of four-digit groups in a rendered code, exported so callers and renderers agree on the shape.

## Derivation

1. `material = blake3_derive_key_from_material("lakko/call/safety-number/v1", seed)` — a one-way, domain-separated BLAKE3 keyed derivation over the raw seed. The dedicated domain context means the displayed material can never collide with any other key derived from the same seed, so showing the code leaks nothing usable about the secret seed.
2. Take the first `SAFETY_NUMBER_GROUPS` (4) of the eight 4-byte chunks of the 32-byte `material`. Each chunk becomes a big-endian `u32` reduced `% 10_000` and zero-padded to four digits (`"{value:04}"`).
3. Join the groups with a single space → `"NNNN NNNN NNNN NNNN"` (e.g. `"6397 5175 0657 3748"` for the all-zero seed).

The rendered length is always exactly `SAFETY_NUMBER_GROUPS * 4 + (SAFETY_NUMBER_GROUPS - 1)` = 19 characters (16 digits + 3 spaces).

## Invariants upheld

- **Deterministic + canonical.** One seed → one exact string, identical across web / iOS / Android — this is what lets two devices on different platforms compare codes at all. Pinned by the `golden_vector_is_stable` test against `GOLDEN_ALL_ZERO = "6397 5175 0657 3748"`, computed firsthand from the all-zero seed; a regression in the hash, the domain context, the byte layout, or the grouping is caught here.
- **One-way display.** The code is a BLAKE3-derive-key over the seed under a distinct domain, so it is a stable fingerprint of the seed that reveals nothing about it and cannot be cross-used as any other keyed material.
- **Diffusion.** A one-bit flip in the seed changes the code (`one_bit_flip_changes_the_number`); distinct seeds yield distinct codes over the tested pair (`distinct_seeds_yield_distinct_numbers`).
- **Fixed shape.** Always four groups of exactly four ASCII digits, space-separated (`has_the_canonical_shape`).

## What this module does NOT do

- **No key material.** It never derives, seals, opens, or exposes anything a caller could use as a key — only a display fingerprint. It never renders the raw seed.
- **No comparison / no MITM decision.** It only produces the string. Whether two codes match, and what to do on a mismatch, is the caller's (client UI's) job; there is no automated equality check here.
- **No transport.** The seed arrives from the caller (the MLS exporter). This module does not fetch, negotiate, or persist it.
- **Not a general fingerprint format.** The domain context and the 4×4-digit shape are specific to the call safety number and intentionally differ from the ASCII Signal-style safety number in `lakko-contacts` (`safety::safety_number`), which serves the contact-verification path.

## Attacker exposure

The input is the trusted per-call seed, not attacker-controlled bytes, so this is not a parser-hardening surface (unlike `header`). The relevant adversary is an **active man-in-the-middle** who has substituted call keys and hopes the two humans do not compare codes. The security property is the collision budget: sixteen decimal digits (~53 bits, `10_000^4`) is comfortably beyond what a MITM could second-preimage inside a call-setup window to make a forged key's code match the honest code, while staying short enough to read aloud. The reduction of each 4-byte chunk `% 10_000` (base-10 rendering of a base-2^32 value) is a small, uniform-enough bias that does not meaningfully shrink that budget for this out-of-band-comparison use.

## References

- C-1 (two parties in the same MLS group + epoch derive byte-identical call seeds — the precondition that makes cross-device comparison meaningful; `docs/security/calls.md`).
- Security invariant #4 (strict E2EE, no silent downgrade) — the safety number is the human-facing MITM-detection tool that backs it.
- Tests (in-module `#[cfg(test)]`): `golden_vector_is_stable`, `is_deterministic`, `has_the_canonical_shape`, `distinct_seeds_yield_distinct_numbers`, `one_bit_flip_changes_the_number`.
