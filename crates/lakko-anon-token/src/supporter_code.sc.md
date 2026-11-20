<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-token/src/supporter_code.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if supporter_code.rs changes and this file does not. -->

# `supporter_code` — supporter codes and their one token request (spec)

**Module:** `crates/lakko-anon-token/src/supporter_code.rs`
**One line:** Parse and print an 80-bit supporter code, and derive the one `beta-place` token request it buys, the same request every time for one epoch.

## Why this module exists

ADR-0097: a Kickstarter backer gets a code by email that holds a beta place past the account cap. The code is redeemed without a session for one VOPRF evaluation; the token it becomes is spent at the first username bind. Auth must give one evaluation per code, yet a client that lost the response must be able to ask again. Deriving the request from the code makes the retry identical, so auth can store the first answer and return it.

## Public contract

- `SupporterCode::generate()` — 10 bytes from the OS CSPRNG (the mint command).
- `parse(text)` — Crockford base32: any case; spaces and dashes skipped; `O` read as `0`, `I` and `L` as `1`; `U` and any other character refused; exactly 16 symbols. Else `BadSupporterCode`. Never panics (proptest).
- `to_display()` — 16 symbols in four groups joined by `-`; `parse(to_display(c)) == c` for every code (proptest).
- `token_request(epoch)` — a one-token `beta-place` `TokenRequest` whose nonce is `BLAKE3-derive-key("lakko/supporter-code/nonce/v1", code ‖ epoch_be)` and whose blinding scalar is RFC 9497's random scalar drawn from a BLAKE3 XOF stream keyed by `BLAKE3-derive-key("lakko/supporter-code/blind/v1", code ‖ epoch_be)`. Same `(code, epoch)` gives the same blinded element; another epoch or code gives another.
- `Debug` prints nothing of the code; the bytes are zeroized on drop.

## Invariants upheld

- One code, one evaluation per epoch on the server side is possible only because the request is reproducible; this module guarantees the reproducibility, auth enforces the "one".
- The token is an ordinary ADR-0054 token of context `beta-place`; its blinding still hides it from the issuer, since the blinding factor is secret to whoever holds the code.

## What this module does NOT do

- No storage, no network, no hash of the code (auth keys that with its pepper).
- Stated cost: whoever holds a code can recompute its token and spend identifier. The maintainer's code list plus auth's database tells which codes were spent, not by which account.

## Attacker exposure

`parse` takes typed text; bounded (stops at a 17th symbol), no allocation beyond the result. Auth parses the code a stranger sends to the redeem route with it.

## References

- ADR-0097 section 1; ADR-0054 decision 2.
- Tests (in-module): `display_and_parse_round_trip_and_forgive_typing`, `known_value_is_pinned`, `same_code_and_epoch_give_the_same_request_and_other_epochs_do_not`, `a_redeemed_code_finalizes_into_a_beta_place_token_that_verifies`, proptests `any_code_round_trips`, `parse_never_panics`.
