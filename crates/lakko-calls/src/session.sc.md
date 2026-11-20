<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/session.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if session.rs changes and this file does not. -->

# `session` — 1×1 call-session state machine (spec)

**Module:** `crates/lakko-calls/src/session.rs`
**One line:** A pure, platform-agnostic state machine that drives a single 1×1 call from dial/ring through connected to ended, translating platform + signaling events into ordered transitions the native WebRTC layer applies.

## Why this module exists

The shared Rust core deliberately does NOT own the live WebRTC `PeerConnection` — each platform uses its own native media stack (Apple `WebRTC.framework`, `org.webrtc:google-webrtc`, browser `RTCPeerConnection`), because the cross-platform Rust-WebRTC crates do not compile to wasm32 beyond pure state and double the native binary. What the core DOES own is the **call orchestration logic**: the `Idle → Outgoing / Ringing → Connecting → Connected → Ending → Ended` progression, the offerer/answerer role split, and the mapping from inbound events to the outbound actions (send offer/answer/candidate/end, apply remote description, show/hide ringer, notify UI). Centralising this in Rust means all three clients share one audited, test-pinned call state machine rather than reimplementing the JSEP-style handshake three times.

This module is orchestration only — it is NOT the media-crypto path. SFrame keying (`sframe::CallKey` / `ParticipantKey`) is armed by platform code once the state reaches `Connected`; this module never touches key material or plaintext media.

## Public contract

- `CallSession { call_id, state, role, media }` — one in-memory session per active call. Constructed idle via `CallSession::idle(call_id)`; `role` and `media` are `None` until the first dial or offer-receive event resolves them.
- `on_event(&mut self, Event) -> Vec<Transition>` — the sole driver. Advances the state machine and returns the transitions the platform must apply, **in order**. Guarantees: it never panics on any event in any state; an event that does not match the current state produces an empty vec (silent drop, see below); once `state == Ended` every further call is a no-op returning empty.
- `apply_remote_signaling(&mut self, &SignalingBody) -> Vec<Transition>` — a convenience adapter that maps a decoded wire `SignalingBody` (`Offer`/`Answer`/`IceCandidate`/`End`) into the matching `Event` and dispatches it. Two special cases: an inbound `IceCandidate` short-circuits directly to a single `ApplyRemoteIceCandidate` transition (never `SendIceCandidate`), and the group-control bodies (`GroupStart`/`GroupJoin`/`GroupLeave`) are dropped silently — they belong to `GroupCallSession`, not the 1×1 machine; so is a `ScreenShare` body (ADR-0083), which moves no call state and goes to the client's share fold, and a `MediaState` body (2026-11-07), the other side's camera and microphone, which the client applies to that person's tile.
- Wire-typed primitives — `MediaKind` (`Audio` / `AudioVideo`, with lossy `from_media_flags` that treats audio-only as the lower-bound fallback), `Role`, `SdpDescription` (opaque `body: String`, never inspected in core), `SdpKind`, `IceCandidate`.

## State machine

Phase ordering is monotonically forward; the only backward-looking edges are the explicit teardown paths into `Ending` / `Ended`.

- **Idle + `LocalDials`** → `Outgoing`, role `Offerer`, media stored, **no wire emission** (the platform now builds the SDP offer; the next event emits it).
- **Idle + `RemoteOffered`** → `Ringing`, role `Answerer`, emits `ApplyRemoteDescription` + `ShowRinger`.
- **Outgoing + `LocalOfferReady`** → emits `SendOffer` (state stays `Outgoing`).
- **Outgoing + `RemoteAnswered`** → `Connecting`, emits `ApplyRemoteDescription`.
- **Outgoing + `IceCandidate`** → emits `SendIceCandidate` (local candidate trickles out).
- **Outgoing + `LocalEnds`** → `Ended` directly (no `Ending` hop, no media up yet), emits `SendEnd` + `NotifyEnded`.
- **Outgoing + `RemoteEnded`** → `Ended` directly, emits a single `NotifyEnded` (peer already gone, nothing to send). Pinned by `remote_end_during_outgoing_emits_single_notify` / `local_end_during_outgoing_sends_then_notifies`.
- **Ringing + `LocalAccepts`** → emits `HideRinger`, **stays `Ringing`** until `LocalAnswerReady` supplies the SDP the platform minted.
- **Ringing + `LocalAnswerReady`** → `Connecting`, emits `SendAnswer`.
- **Ringing + `LocalDeclines`** → `Ended`, emits `HideRinger` + `SendEnd(Declined)` + `NotifyEnded(Declined)`.
- **Ringing + `RemoteEnded`** → `Ended`, emits `HideRinger` + `NotifyEnded`.
- **Connecting + `IceCandidate`** → emits `SendIceCandidate`.
- **Connecting + `IceConnected`** → `Connected`, emits `NotifyConnected`.
- **Connecting + `IceFailed`** → `Ended`, emits `SendEnd(Error)` + `NotifyEnded(Error)`.
- **Connected + `IceCandidate`** → emits `SendIceCandidate` (late candidates / ICE-restart).
- **Connected + `IceFailed`** → stays `Connected`, emits nothing — the platform may recover with an ICE restart and raise a follow-up event; the state machine does not tear down on the first failure.
- **Ringing / Connecting / Connected + `LocalEnds`** → `Ending`, emits `SendEnd` + `NotifyEnded`.
- **Connecting / Connected + `RemoteEnded`** → `Ended`, emits `NotifyEnded`.
- **Ending + `RemoteEnded` / `IceFailed` / `IceConnected`** → `Ended`, emits nothing (drain the teardown handshake).
- **Everything else** → empty vec (silent drop).

## Invariants upheld

- **`Ended` is terminal and absorbing** — no event transitions out of it, and every call in that state returns empty. Verified by `stale_event_in_ended_is_dropped`.
- **No wire emission on dial** — `LocalDials` only sets local state; the `SendOffer` is deferred to `LocalOfferReady` so the offer carries the SDP the platform's PeerConnection actually produced. Verified by `outgoing_happy_path`.
- **Accept is two-phase** — `LocalAccepts` hides the ringer but does not advance to `Connecting`; only `LocalAnswerReady` (carrying real SDP) does. This prevents sending an answer before the platform has one. Verified by `incoming_happy_path`.
- **Inbound remote candidates are applied, never re-sent** — `apply_remote_signaling` for an `IceCandidate` always yields `ApplyRemoteIceCandidate` regardless of state, so a received candidate is never mistakenly echoed back onto the wire. Verified by `apply_remote_signaling_candidate_in_any_state`.
- **Media flags survive the round-trip through the enum** — `MediaKind::into_media_flags` / `from_media_flags` bridge the typed enum and the wire bitmap; unknown future bits demote conservatively (video present → `AudioVideo`, else `Audio`).

## Failure modes

- **No errors, no panics.** `on_event` is infallible and returns a (possibly empty) vec; there is no `Result`. The `self.media.unwrap_or(MediaKind::Audio)` in the Outgoing `LocalOfferReady` arm is a total fallback, not a panicking `unwrap`.
- **Silent drop is the failure posture.** An event that does not match the current `(state, event)` pair returns an empty vec. This is a deliberate design choice mirroring the canonical WebRTC/JSEP signaling-state-machine UX: stale candidates, races, and post-end follow-ups are dropped rather than surfaced as loud errors that would force every call site into noisy match arms. The cost is that a genuinely malformed or out-of-order event is indistinguishable from an expected stale one at this layer — callers get no diagnostic.

## What this module does NOT do

- **No WebRTC / no media.** It never holds a `PeerConnection`, never generates or applies SDP itself (SDP is opaque `String`), never touches ICE transports.
- **No cryptography and no SFrame keying.** Reaching `Connected` is the *signal* for platform code to arm SFrame from the MLS exporter; this module derives no keys and sees no plaintext frames.
- **No `call_id` matching.** `apply_remote_signaling` maps the body to an event without checking the body's `call_id` against `self.call_id`. Rejecting wrong-`call_id` / wrong-length bodies is the responsibility of the signaling layer and the group-call path (see C-12 / C-14 in `group_call.rs` / `signaling.rs`), not this 1×1 state machine — the dispatcher is expected to have already routed only matching bodies here.
- **No group-call orchestration.** Multi-party `GroupStart`/`GroupJoin`/`GroupLeave` bodies are dropped; they belong to `GroupCallSession`.
- **No timers.** "Timeout before pickup" and dead-call timeouts are the platform's concern; the machine only reacts to events it is fed.

## Attacker exposure

This module consumes events that a platform dispatcher synthesises from wire `SignalingBody` values relayed over the zero-trust DS journal, so the SDP/candidate strings and media flags are peer-controlled. Because SDP and candidate bodies are treated as opaque `String`s and never parsed here, there is no parser to attack at this layer; the machine's only obligation is that no `(state, event)` combination panics or loops, which the exhaustive match + silent-drop fallthrough guarantee. There is no fuzz target for this module (it has no byte parser of its own); the attacker-facing byte-parsing surface lives in `crate::signaling` and `crate::header`.

## Notable code observations (findings)

- **Teardown asymmetry.** `LocalEnds` from `Outgoing` goes straight to `Ended`, whereas from `Ringing`/`Connecting`/`Connected` it goes to the intermediate `Ending` state first. Deliberate (no media handshake to drain pre-connect), but the two teardown shapes differ.

(A previously-noted duplicate `NotifyEnded` on the Outgoing `RemoteEnded` path was fixed: the combined `(Outgoing, LocalEnds | RemoteEnded)` arm was split into two arms, so `RemoteEnded` now emits a single `NotifyEnded`. Regression-pinned by the two tests named in the state-transition list above.)

## References

- ADR-0011 (Rust SFU), ADR-0013 (SFrame AAD) — the media layers this session orchestrates around but does not implement.
- Related invariants (enforced in sibling modules, NOT here): C-12 (wrong-`call_id` bodies dropped, `group_call.rs`), C-14 (`call_id` length enforced at parse, `signaling.rs`).
- Tests (in-module `#[cfg(test)]`): `outgoing_happy_path`, `incoming_happy_path`, `incoming_decline_path`, `local_hangup_during_connected`, `ice_failure_during_connecting`, `stale_event_in_ended_is_dropped`, `apply_remote_signaling_offer`, `apply_remote_signaling_candidate_in_any_state`.
