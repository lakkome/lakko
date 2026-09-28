<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-device-link/src/lib.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if lib.rs changes and this file does not. -->

# `lakko-device-link` — the code-typed add-device exchange (spec)

**Module:** `crates/lakko-device-link/src/lib.rs`
**One line:** The signed-in device shows a six-digit code it chose, the person types it on the new device, and SPAKE2 over that code seals the new device's pairing payload so a relay gets one guess per request.

## Why this module exists

The camera-less another-device route (ADR-0049 section 2) relays the new device's pairing payload through auth. The earlier comparison code was SHA-256 of that payload, and the payload carries a free 32-byte rendezvous pseudonym, so a relay could grind a payload of its own until the codes matched and have its device added. Here the code never leaves the old device except through the person, and the key both devices use is a password-authenticated one: a relay learns nothing passively and gets one online guess per exchange.

## Public contract

- `LinkContext::new(request_id, login_handle)` — 16 and 32 bytes, else `BadInput`. Both SPAKE2 identities and the transcript carry it.
- `LinkCode::generate(rng)` — six uniformly random digits: a `u32` sample at or above `4_294_000_000` is redrawn, the rest reduced mod one million. `LinkCode::parse(input)` — ASCII digits only, spaces and `-` ignored, exactly six, else `InvalidCode` (a full-width or Arabic-Indic digit is refused). `as_str()` gives `"471903"`, `grouped()` gives `"47 19 03"`. Held in `Zeroizing`; `Debug` is redacted.
- `Responder::start(context, rng)` — the old device (SPAKE2 side B): a fresh code and `pake_b` (33 bytes, `pake_message()`).
- `initiate(context, typed_code, pake_b, payload, rng)` — the new device (side A), in one step: `Sealed { pake_a, sealed_payload, confirm_a }` and a `ConfirmationCheck`. The payload is 1..=2048 bytes (`BadInput`); a `pake_b` of the wrong length or side, or not a curve point, is `Malformed`.
- `Responder::accept(self, pake_a, sealed, confirm_a)` — consumes the responder. Shape checks first (`Malformed`), then the SPAKE2 finish (`Malformed` for a side-B or non-point message), then the constant-time compare of `confirm_a` (`WrongCode`), then the AEAD open (`Malformed` if it fails under a key whose confirmation verified, or opens to nothing). On success: `Accepted { payload, confirm_b }`.
- `ConfirmationCheck::verify(confirm_b)` — constant time; `WrongCode` on a mismatch, `Malformed` on a wrong length.
- `LinkError::code()` — `wrong-code`, `malformed`, `invalid-code`, `bad-input`, `internal`: the stable strings both FFI surfaces carry.

## Invariants upheld

- One guess per request on the old device: `accept` takes `self`.
- A message moved to another request id or another login handle yields a different key, so it fails as `WrongCode`.
- A change to any of `pake_a`, `pake_b`, the sealed payload or `confirm_a` fails: the transcript binds both SPAKE2 messages, `confirm_a` binds the SHA-256 of the sealed payload, and the AEAD binds the transcript as associated data.
- The payload key seals exactly one message, so its nonce is derived, not random.
- The derived keys and the code this crate owns are zeroized on drop.

## What this module does NOT do

- No network and no server logic; auth relays the four values opaquely (`services/auth/src/device_request.rs`).
- It does not stop a server that fabricates many requests from getting one guess per request the person's device answers; the client limits how many it answers.
- It does not zeroize the `spake2` crate's internal copy of the password and its ephemeral scalar (that crate offers no way to); they are freed, not wiped.
- It does not parse the pairing payload; the old device decodes it with `lakko_mls::pairing::decode` against its own handle.

## References

- ADR-0049 section 2 (amended 2026-09-28); ADR-0056 (the payload it seals); `docs/ux/identity-recovery.md` section 4.
- Tests: `right_code_round_trips_and_confirms_both_ways`, `wrong_code_is_refused`, `a_relay_that_changes_any_message_fails`, `a_sealed_payload_moved_to_another_request_fails`, `side_confusion_and_bad_shapes_are_malformed`, `code_generation_rejects_at_the_bound`, `code_generation_is_unbiased_over_a_large_sample`, the three proptests; fuzz target `device_link_accept`.
