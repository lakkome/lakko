<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-contacts/src/conversation_reentry.rs; keep in lock-step with the source (just spec-check). -->

# `conversation_reentry` — a behind device's request to come back (spec)

**Module:** `crates/lakko-contacts/src/conversation_reentry.rs`   **One line:** the
bounded envelope a device that fell behind a conversation sends to be brought back,
and its sealed knock-row form for a group (ADR-0091 section 3).

## Why this module exists

The request travels by three routes (a DeviceGroup service envelope, a sealed
mailbox row in a one-to-one, a sealed knock row in a group). One codec serves all
three so a re-admitter reads the same fields whichever way it came.

## Public contract

- `ConversationReentry { group_id(32), epoch_left, gap_from_ms, gap_key(32),
  key_package }`. `gap_key` is the public half of a fresh X25519 gap key the device
  minted for this request (`gap_history::GapKey`): the gap's history is sealed to it
  alone (security review 2026-11-21).
- `compose_conversation_reentry`: `"lakko/conversation-reentry/v1\0" ‖ 0x02 ‖
  group_id ‖ epoch_left(8, BE) ‖ gap_from_ms(8, BE) ‖ gap_key(32) ‖ kp_len(2, BE) ‖
  key_package`. Refuses an empty KeyPackage or one above 4096 bytes, exactly what
  decode refuses.
- `try_unwrap_conversation_reentry` / `decode_conversation_reentry`: `Ok(None)`
  (try) or `WrongDomain` (decode) without the separator; the version is checked
  before any length and only `0x02` is accepted (version 1, which carried no gap
  key, is `UnknownVersion(1)`); the KeyPackage length is bounded before it is
  copied; the envelope must be exact (114 to 4209 bytes). Whatever decodes
  re-composes to the same bytes.
- `seal_reentry_knock(knock_pk, version, envelope)`: the envelope, length-prefixed
  and zero-padded to the same two row sizes as a join request (4 KiB, 16 KiB),
  sealed with the knock labels under the AAD domain
  `lakko/group/reentry-knock-row/v1\0`; the row starts with the join row's version
  byte and the knock version. Refuses an envelope that does not decode.
- `open_reentry_knock(seed, row)`: refuses another shape (`BadRow`), an AEAD
  failure (`NotOpened`, including any join request), non-zero padding or an
  overrunning prefix (`BadFraming`), then decodes the envelope.

## Invariants upheld

- The DS sees a re-entry knock row exactly as it sees a join request (size, first
  three bytes); only a holder of the knock seed learns which it is.
- A join request never opens as a re-entry and a re-entry row never opens as a join
  request (distinct associated data).
- The envelope names no account; the KeyPackage inside it carries the device's
  binding, as any KeyPackage does.

## Failure modes

`ConversationReentryError::{WrongDomain, BadLength, UnknownVersion(u8),
BadKeyPackage, BadRow, NotOpened, BadKey, BadFraming}`, each with a frozen `code()`.
No panic on any input.

## What this module does NOT do

It does not validate the KeyPackage or its marker (`lakko_mls::Group::
reentry_request_target` does), decide whether to admit (`reentry_admission_verdict`),
or trust `gap_from_ms`: the re-admitter clamps it with
`gap_history::gap_history_start` before choosing rows. It does not check the gap
key; a low-order one makes the re-admitter's seal fail, so no history is sent.

## Attacker exposure

Route 2 rows are written by anyone holding a reach grant and route 1 / route 3
plaintexts by a sibling device or a knock writer; the decoder is bounded and exact.
Fuzz target `conversation_reentry_decode` (also drives the opened-row parser).

## References

ADR-0091 sections 3 and 7 and "Phase 2 S0-S2, as built"; C-93; `knock.rs`;
`gap_history.rs`.
