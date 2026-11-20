<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-token/src/token.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if token.rs changes and this file does not. -->

# `token` — anonymous-token wire codec and verifier (spec)

**Module:** `crates/lakko-anon-token/src/token.rs`
**One line:** The fixed 106-byte token, the VOPRF input it commits to, its spend identifier, and the check a consuming service runs on the bytes an anonymous request carries.

## Why this module exists

ADR-0054 decision 2 replaces the per-request proof of work on the anonymous routes (lookup, knock, invite consume, push registration) with tokens a signed-in account draws from a quota at `services/auth` and spends anonymously elsewhere. A consuming service receives the token bytes from anyone and must decide, before touching any state, whether they are a token the issuer produced for this action in a live epoch. This module is that decision, written once for every verifier.

## Public contract

- `Token { context, epoch, nonce, output }` with `encode()` and `decode(bytes)`. `decode` is bounded and fail-closed: the length must be exactly `TOKEN_LEN`, then the version byte must be `TOKEN_VERSION`, then the context byte must name a `TokenContext`; nothing is allocated. Every decoded token re-encodes to the identical bytes (the layout has no slack).
- `Token::spend_id()` / `SpendId` — BLAKE3 derive-key (`"lakko/anon-token/v1 spend-id"`) over `version || context || epoch || nonce`. The output is deliberately excluded: the fields that decide validity decide the identifier, so no second encoding of one token can be spent separately. It names no account.
- `token_input(context, epoch, nonce)` — the VOPRF input, `"lakko/anon-token/v1 input" || version || context || epoch_be || nonce`. The client blinds exactly this; the verifier evaluates exactly this.
- `verify_token(master, bytes, expected, now_unix_secs, window_secs) -> VerifiedToken` — checks, in order: `decode`; the context equals `expected`; the epoch is the current or the previous one at `now` under `window_secs`, or under the context's own fixed window when it has one (`beta-place`, 30 days, ADR-0097; `TokenContext::window_secs`), so every verifier agrees on that context's epoch whatever it is configured with; the output equals the VOPRF evaluation of `token_input` under the `(context, epoch)` key derived from `master`, compared in constant time (`subtle`). Returns the context, epoch and spend identifier.
- `Token`'s `Debug` prints the context and epoch only, never the nonce or output (what makes a token spendable).

## Wire layout

```text
version(1) = 0x01 || context(1) || epoch(8, big-endian) || nonce(32) || output(64)
```

## Invariants upheld

- A token verifies only for the context and the epoch it was issued for: both are bound twice, by the key derivation and by the input. Relabelling either byte yields `InvalidToken`, and presenting a token at another context yields `WrongContext`.
- A token lives at most two epochs (its own and the next); an older or a future epoch is refused before any key is derived.
- Verification is pure. It does not stop a second spend; the caller must insert the returned spend identifier into its spent-set atomically (`lakko-anon-token-server::spend`) before doing the work the token pays for.

## Failure modes

`Malformed`, `UnknownVersion`, `UnknownContext` (decode); `WrongContext`; `InvalidWindow` (zero window); `EpochOutOfWindow`; `InvalidToken` (output mismatch); `Internal` (a VOPRF failure valid inputs cannot reach). No path panics: all slicing is over a `&[u8; TOKEN_LEN]` with constant bounds, and the epoch arithmetic cannot overflow.

## What this module does NOT do

- It keeps no state: no spent-set, no clock, no key cache.
- It never learns or returns anything about the account that drew the token.
- It does not issue: issuance is `issuer.rs`, blinding and finalizing `client.rs`.

## Attacker exposure

Every byte of the input is attacker-controlled (the verifier runs on anonymous requests). The parser is fixed-length, and the verify cost after a successful parse is one key derivation and one VOPRF evaluation. Fuzz target `token_verify` (`fuzz_api::decode_and_verify`) drives decode plus the full verifier for every context under a fixed master and clock, and asserts canonical re-encoding.

## References

- ADR-0054 decision 2; RFC 9497 (VOPRF, Ristretto255-SHA512).
- Tests: in-module `codec_round_trips_with_the_documented_layout`, `decode_is_fail_closed`, `spend_id_depends_on_every_validity_field_and_not_the_output`, `debug_never_prints_nonce_or_output`; `tests/round_trip.rs` (context / epoch relabelling, wrong master, forged output, bit-flip proptest).
