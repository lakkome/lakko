<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-token/src/wire.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if wire.rs changes and this file does not. -->

# `wire` — the HTTP text form of tokens and of the issuance exchange (spec)

**Module:** `crates/lakko-anon-token/src/wire.rs`
**One line:** The header a spending request carries a token in, and the one base64 alphabet every binary field of the exchange travels as.

## Why this module exists

ADR-0054 S3 puts anonymous tokens on the wire at three verifying services (discovery, the DS, push-gw) and on two FFI surfaces (UniFFI, wasm). A header name or an alphabet spelled five times drifts; this module spells it once.

## Public contract

- `TOKEN_HEADER = "x-lakko-anon-token"` — the request header that spends a token.
- `TOKEN_HEADER_VALUE_LEN = 142` — base64url without padding of the 106-byte token.
- `encode_b64url(bytes)` — base64url, no padding: the text form of a token, a blinded element, an evaluation, the DLEQ proof and a public key.
- `decode_b64url_fixed::<N>(text) -> Option<[u8; N]>` — the text length must be exactly that of `N` bytes before anything is decoded; any other length, a character outside the URL-safe alphabet, padding, or non-zero trailing bits yields `None`.
- `Token::to_header_value()` — the header value that spends the token.
- `decode_header_value(value) -> Result<[u8; TOKEN_LEN]>` — `decode_b64url_fixed::<TOKEN_LEN>`, refusing with `Malformed`. It does not look inside the token; `verify_token` does.

## Invariants upheld

- The header decoder is bounded: a value of the wrong length is refused before a byte is decoded, so its cost is independent of what an anonymous caller sends.
- Encoding is canonical: every value that decodes re-encodes to the identical text (the fuzz harness asserts it), so one token has exactly one header value.
- No surrounding whitespace is tolerated and the standard alphabet (`+`, `/`) is refused.

## What this module does NOT do

- It never verifies, spends or records a token.
- It does not read HTTP headers itself; the service gate (`lakko-anon-token-server::gate`) hands it the value.

## Attacker exposure

Every header value is attacker-controlled. `fuzz_api::decode_and_verify` runs `decode_header_value` on the fuzz input read as UTF-8 and asserts canonical re-encoding, alongside the byte-level token verifier.

## References

- ADR-0054 decision 2 and its S3 note.
- Tests: in-module `header_value_round_trips_and_has_the_pinned_length`, `header_value_decode_is_fail_closed`, `fixed_decode_checks_the_length`; `tests/round_trip.rs` `the_base64url_exchange_round_trips_and_the_header_value_verifies`.
