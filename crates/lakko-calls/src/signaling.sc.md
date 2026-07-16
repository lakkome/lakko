<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/signaling.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if signaling.rs changes and this file does not. -->

# `signaling` — in-chat call-signaling codec (spec)

**Module:** `crates/lakko-calls/src/signaling.rs`
**One line:** Compose / parse the typed `SignalingBody` events (offer, answer, ICE, end, group start/join/leave) that ride the existing conversation MLS group as plaintext application-message payloads.

## Why this module exists

In-chat calls reuse the chat group's MLS epoch and DS journal rather than opening a new transport (plan §5, ADR-0001 "Звонки"). This module is the sole codec for the call-control events that flow over that channel: it turns a typed `SignalingBody` into the plaintext proto bytes the caller wraps with `lakko_mls::Group::create_application_message`, and turns the bytes that fall out of an `Incoming::Application` back into a typed body.

The codec is deliberately **plaintext-only** — it performs no encryption of its own. The MLS envelope is the entire privacy and authenticity layer; this module packs/unpacks integers and strings inside it. Per-peer SDP/ICE for SFU group calls still flows participant ↔ SFU directly; only the call-control lifecycle (`GroupStart` / `GroupJoin` / `GroupLeave`) rides MLS here.

## Public contract

- `SignalingBody` — the seven-variant typed event: `Offer`, `Answer`, `IceCandidate`, `End` (1×1 lifecycle) and `GroupStart`, `GroupJoin`, `GroupLeave` (SFU group-call lifecycle). Mirrors the `CallSignalingPayload.body` oneof in `schemas/mls-app/call_signaling_v1.proto`.
- `SignalingBody::call_id() -> &CallId` — every variant carries a 16-byte per-call id, minted by the offer/initiator side. This accessor is the primitive the receiver uses to match an incoming body to the active call.
- `SignalingBody::is_group_control() -> bool` — `true` for exactly `GroupStart` / `GroupJoin` / `GroupLeave`. This is the **canonical client-side dispatcher gate** that routes a body to the 1×1 (`CallSession`) vs. group (`GroupCallSession`) state machine (C-11). The routing/drop logic itself lives in those consumers, not here — this module only supplies the discriminator and the id.
- `compose_signaling_payload(&SignalingBody) -> Result<Vec<u8>>` — encodes to plaintext proto bytes. Practically infallible: every field is fixed-shape, so the prost encode error is unreachable in normal use (returned as `Error::Crypto` if it ever fired).
- `parse_signaling_payload(&[u8]) -> Result<SignalingBody>` — the inverse. Decodes, enforces the 16-byte `call_id` length, and folds unknown `EndReason` codes forward.
- `EndReason` — pinned numeric discriminator (`HungUp=0`, `Busy=1`, `Declined=2`, `TimedOut=3`, `Error=4`). For `GroupLeave` only `HungUp` / `TimedOut` / `Error` are meaningful; `Busy` / `Declined` are 1×1-only.
- `MediaFlags { audio, video }` — a 2-bit wire bitmap (bit 0 = audio, bit 1 = video) with `into_bits` / `from_bits` round-tripping.

## Wire layout

Proto-encoded `CallSignalingPayload { body: oneof }`; this module never hand-rolls varint framing (prost owns it). Two fields are hand-validated on decode:

- **`call_id`** — carried on the wire as a length-delimited `bytes`; the codec re-narrows it to a fixed `[u8; 16]` and rejects any other length (C-14).
- **`EndReason` / `media_flags`** — carried as `u32`; mapped through `EndReason::from_proto` (unknown → `HungUp`) and `MediaFlags::from_bits` (only bits 0/1 read, higher bits ignored).

## Invariants upheld

- **C-13 — unknown `EndReason` folds to `HungUp`.** `EndReason::from_proto` maps any discriminator outside `1..=4` to `HungUp` (the `_ =>` arm), so a future schema adding reason codes 5..N lands gracefully on an older receiver instead of erroring the call down. Pinned by `unknown_end_reason_folds_to_hung_up`.
- **C-14 — `call_id` length strictly enforced at parse time.** The inner `take_call_id` closure returns `Error::Crypto("call_id length: expected 16, got N")` on any non-16-byte id — never an index panic. Pinned by `wrong_call_id_length_rejected`.
- **C-11 (supporting) — group-control discriminator.** `is_group_control()` is the gate the 1×1 dispatcher relies on to drop `GroupStart/Join/Leave` rather than cross-feeding them into the 1×1 state machine. Pinned here by `is_group_control_false_for_one_on_one_bodies`; the actual silent-drop is verified in the consumer modules.
- **C-12 (supporting) — `call_id` matching.** The `call_id()` accessor is what `GroupCallSession` uses to drop wrong-`call_id` bodies; the drop itself is enforced and tested there, not in this codec.

## Failure modes

- `Error::Crypto("decode CallSignalingPayload: …")` — the prost decode failed on malformed bytes.
- `Error::Crypto("CallSignalingPayload missing body")` — the oneof is absent (e.g. an older client reading a future-only variant). The doc-comment invites the caller to treat this as drop-silently.
- `Error::Crypto("call_id length: expected 16, got N")` — the embedded id is not 16 bytes (C-14).
- `Error::Crypto("encode CallSignalingPayload: …")` — the (unreachable) encode failure.
- **No panics.** Decode never indexes unchecked; the only fixed-array copy is guarded by the preceding length check.

## What this module does NOT do

- **No encryption, no authentication, no integrity check of its own.** It emits and consumes plaintext. All confidentiality/authenticity is the MLS application-message layer's job. A body that reaches `parse_signaling_payload` has already been MLS-authenticated as coming from a group member; this codec adds no further trust.
- **No dispatch / routing / state-machine advance.** It classifies (`is_group_control`) and exposes the id (`call_id`) but does not decide 1×1-vs-group, does not drop wrong-`call_id` bodies, and does not touch any call ledger — those are `CallSession` / `GroupCallSession` (C-11 / C-12 enforcement lives there).
- **No `call_id` minting or randomness.** The caller supplies the 16-byte id.
- **No transport.** It never touches the DS journal or the SFU — it only hands bytes to / from the caller.

## Attacker exposure

`parse_signaling_payload` runs on bytes that arrive as `Incoming::Application` from the zero-trust DS journal after MLS decrypt/auth — so the sender is an authenticated group member, but a **malicious or compromised member can craft arbitrary payload bytes** for a call in a shared chat group. The bound is: parse must stay panic-free and reject malformed input as a typed `Error`, which C-14 (length) and the prost decoder together provide. The forward-compat folds (C-13, missing-body) keep a hostile or stale payload from tearing down an unrelated call.

Fuzzed by `crates/lakko-calls/fuzz/fuzz_targets/signaling_payload_decode.rs` (asserts no-panic on arbitrary bytes + round-trip stability of every accepted body, which transitively exercises the 16-byte `call_id` invariant). The hand-written surface is small — one length-checked fixed-array copy over a prost-decoded struct plus the `MediaFlags`/`EndReason` folds.

## References

- ADR-0001 (MLS-only — calls reuse the chat group's epoch + DS journal)
- Invariants C-11 (1×1 dispatcher does not cross-feed group-control bodies — gate provided here), C-12 (wrong-`call_id` dropped — id accessor provided here), C-13 (`EndReason` unknown → `HungUp`), C-14 (`call_id` length enforced), in `docs/security/calls.md`
- Tests (in-module `#[cfg(test)]`): `unknown_end_reason_folds_to_hung_up`, `wrong_call_id_length_rejected`, `is_group_control_false_for_one_on_one_bodies`, `group_start_round_trips` / `group_join_round_trips` / `group_leave_round_trips_each_reason`, the 1×1 `*_round_trips` cases, `media_flags_round_trip_bits`
