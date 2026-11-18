<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/key.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if key.rs changes and this file does not. -->

# `key` — per-period issuer key and periods (spec)

**Module:** `crates/lakko-anon-cred/src/key.rs`
**One line:** Derive the issuer key for a period from the master secret auth and channels share, publish its public half, and define periods and the accepted window.

## Why this module exists

The issuer (`services/auth`) and the verifier (`services/channels`) must hold the same key for a period with no key table and no distribution protocol (ADR-0090 4.4, the ADR-0054 anonymous-token pattern). Deriving each period's key from one master secret gives that, and the key rotates on its own each period.

## Public contract

- `MasterSecret` — 32 bytes, wiped on drop, `Debug` prints nothing. `from_hex` takes exactly 64 hex characters (either case, whitespace trimmed), else `InvalidMasterSecret`. It is a different secret from the anonymous-token master: discovery, the DS and push-gw hold that one and must not be able to mint credentials.
- `IssuerSecretKey::derive(master, period)` — each of `x0`, `x0t`, `x1`, `x2` is `BLAKE3-derive-key("lakko/anon-cred/v1 issuer-key", master || version || period_be || name)` in XOF mode, 64 bytes, wide-reduced; a zero scalar is `ZeroScalar`. Deterministic; distinct master or period give distinct keys. Scalars are wiped on drop; `Debug` shows the period only.
- `IssuerPublicKey` — `C_x0 || X1 || X2`, 96 bytes, published under its period (the period is not in the bytes). `decode(period, bytes)` requires 96 bytes and three canonical non-identity points.
- `PERIOD_SECS` = 30 days (chosen, not measured); `period_at(unix_secs) = floor(unix_secs / PERIOD_SECS)`; `period_accepted(presented, current)` is true for the credential's own period and the next one only (the ADR's grace at a boundary), never for a future period, without overflow.

## Invariants upheld

- One key per period for every holder of the master; no per-account key exists, which is what the client's check of the issuer's proof against the published key relies on.
- Changing `PERIOD_SECS` renumbers periods and invalidates every credential; both services must use the same value.

## Attacker exposure

A holder of the master can mint credentials and cast votes (ADR-0090 4.4, accepted). It learns no account and no secret `s`.

## References

- ADR-0090 4.2, 4.4; ADR-0054 decision 2.
- Tests: `derivation_is_deterministic_and_separated`, `public_key_round_trip`, `periods`, `master_secret_hex`; `golden_values_are_pinned` in `tests/protocol.rs`.
