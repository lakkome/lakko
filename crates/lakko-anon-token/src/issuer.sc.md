<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-token/src/issuer.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if issuer.rs changes and this file does not. -->

# `issuer` — anonymous-token issuance key (spec)

**Module:** `crates/lakko-anon-token/src/issuer.rs`
**One line:** Derive the VOPRF key for a `(context, epoch)` from one shared master secret, publish its public key, and evaluate a blinded batch with one DLEQ proof.

## Why this module exists

The issuer (`services/auth`) and every verifier (discovery, the DS, push-gw) must hold the same key for a `(context, epoch)` without a key table or a distribution protocol, and keys must rotate on their own. Deriving every key from one master secret gives both: all holders compute the same key, and the key changes when the epoch does.

## Public contract

- `MasterSecret` — 32 bytes, wiped on drop, `Debug` prints nothing of the value. `from_hex` accepts exactly 64 hex characters (either case, surrounding whitespace trimmed) and refuses anything else with `InvalidMasterSecret`.
- `IssuerKey::derive(master, context, epoch)` — deterministic. `seed = BLAKE3-derive-key("lakko/anon-token/v1 issuance-key-seed", master || version || context || epoch_be)`, then RFC 9497 `DeriveKeyPair(seed, info)` in VOPRF mode with `info = "lakko/anon-token/v1" || version || context || epoch_be`. Distinct master, context or epoch give distinct keys. The only error, `Internal`, requires all 256 counter-indexed hash-to-scalar attempts of `DeriveKeyPair` to yield zero.
- `IssuerKey::public_key()` — the compressed Ristretto255 point a client checks the proof against. Pinned for one `(master, context, epoch)` by a golden test computed by this implementation.
- `IssuerKey::issue(blinded)` — refuses an empty batch or one larger than `MAX_BATCH` (`BadBatchSize`) and any element that is not a canonical, non-identity Ristretto255 encoding (`BadElement`), all before evaluating; otherwise returns one evaluation per element in order plus one batched DLEQ proof.
- `validate_blinded_batch(blinded)` — the same checks without evaluating, so the issuer can refuse a malformed request before charging quota.

## Invariants upheld

- One key per `(context, epoch)` for every holder of the master; no per-account key exists, which is what the client-side DLEQ check relies on to detect tagging.
- VOPRF mode only (never plain OPRF): every evaluation carries a proof.
- Secret material (master, seed, derivation buffer) sits in `Zeroizing` buffers.

## What this module does NOT do

- It does not count quota or know accounts (that is `services/auth` with `lakko-anon-token-server`).
- It does not decide which epoch is current; the caller passes it.
- It exposes no `voprf` type in its public API.

## Attacker exposure

`issue` runs on blinded elements a signed-in client sends; the batch cap bounds the work and every point is validated before use. A compromised holder of the master can mint tokens (a quota bypass, ADR-0054) but learns no key or content of anyone.

## References

- ADR-0054 decision 2; RFC 9497 section 3.2.1 (`DeriveKeyPair`).
- Tests: `derivation_is_deterministic_and_separated`, `golden_public_key_is_pinned`, `master_secret_hex_parsing`, `issue_refuses_bad_batches`; `tests/round_trip.rs`.
