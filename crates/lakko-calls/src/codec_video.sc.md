<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/codec_video.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if codec_video.rs changes and this file does not. -->

# `codec_video` — video codec ladder + negotiation hint (spec)

**Module:** `crates/lakko-calls/src/codec_video.rs`
**One line:** Pin the cross-platform video-codec preference list, the offer/answer intersection rule, and the SDP `mimeType` / `fmtp` strings — pure data, no encoder.

## Why this module exists

The encode/decode pipeline lives in each platform's native WebRTC stack, but the **preference list** advertised in SDP and the **intersection rule** the negotiation runs must agree byte-for-byte across web / iOS / Android. A drift there surfaces as a silent quality regression (both support AV1, one still lands on VP9) or, worse, no common codec at all. This module is the single shared source of the codec choice so no platform can drift its ladder independently. It is the video sibling of `crate::codec_audio` (whose Opus path needs no ladder — Opus is universal).

## Public contract

- `VideoCodec` — a three-variant enum `{ H264 = 0, Vp9 = 1, Av1 = 2 }`. The **discriminant IS the preference rank**: higher value = more preferred. The derived `Ord` therefore orders codecs by preference, and the intersection picker leans on that.
- `VideoCodec::preference_order() -> [Self; 3]` — the canonical advertise order, highest-rank first: `[Av1, Vp9, H264]`. Every platform advertises this order; this array is the load-bearing contract.
- `sdp_mime_type() -> &'static str` — the WebRTC `mimeType` (`video/AV1`, `video/VP9`, `video/H264`, uppercase per the RTP payload RFCs).
- `short_name() -> &'static str` — lowercase diagnostic/wire id (`av1`, `vp9`, `h264`); these are the exact strings `schemas/media/v1.proto`'s `CodecCapability.video_codecs` expects.
- `supports_svc() -> bool` — `true` for AV1 and VP9, `false` for H.264 (simulcast-only). A codec-level capability table pinned here so the platform layer doesn't grow its own.
- `sdp_fmtp_string() -> String` — the `a=fmtp:` payload value (without the `a=fmtp:<pt> ` prefix the caller prepends), with codec-specific parameters baked in (AV1 `level-idx=5;profile=0;tier=0`; VP9 `profile-id=0`; H.264 Constrained Baseline 3.1 `...;profile-level-id=42e01f`). Only this method allocates.
- `pick_intersection(offer, answer) -> Option<Self>` — the highest-rank codec both slices advertise, chosen by iterating the **canonical** preference order (so the answer is independent of the input slices' ordering). `None` only when the intersection is empty.
- `VideoCapability { codecs, hardware_av1, svc_enabled }` — a participant's advertised capability; `default_v1()` seeds the full ladder, `svc_enabled = true`, `hardware_av1 = false` (the caller flips it after probing the platform). `to_proto_strings()` renders `codecs` to short names in order.

## Invariants upheld

- **Preference order is `[Av1, Vp9, H264]`** and the enum's `Ord` matches that rank (`Av1 > Vp9 > H264`) — pinned by `preference_order_is_av1_vp9_h264` and `ord_matches_preference_rank`.
- **Intersection is order-independent:** the pick derives from the canonical order, not the order the offer/answer slices happen to list — pinned by `intersection_order_is_independent_of_input_order`.
- **Short names match the proto wire convention** (`av1`/`vp9`/`h264`) so a rename can't silently desync the `CodecCapability.video_codecs` field — pinned by `short_names_match_proto_field_convention` and `to_proto_strings_emits_short_names_in_order`.
- **`fmtp` strings are pinned to the WebRTC-canonical profiles** (H.264 `42e01f` Constrained Baseline 3.1, AV1 Main/Level 5, VP9 profile 0) so a drift that would break iOS-Safari interop surfaces in a test rather than in a call — pinned by the three `*_fmtp_advertises_*` tests.

These are correctness/interop contracts, not security invariants — this module holds no crypto and there is no `C-*` / `I-*` id for it.

## Failure modes

- No fallible parsing and no error type. `pick_intersection` returns `Option` (`None` = empty overlap, expected never to fire in production because every client ships H.264 as the floor); every other method is total. No panics; no `unwrap`/`expect`; the only allocation is the `String`/`Vec` these builders return.

## What this module does NOT do

- **No encoding or decoding.** It never touches a frame — it only names the codec the native stack will use.
- **No SDP negotiation.** The real offer/answer intersection happens inside the platform WebRTC stack; `pick_intersection` exists only to drive **pre-SDP UI hints** ("expect 480p — the other side only supports H.264"), not to select the wire codec.
- **No SVC layer selection or runtime layer switching.** It pins the codec *choice* and the static SVC-capability answer; the bandwidth-driven layer state machine is `crate::quality`'s concern (deferred to a later sprint per the module docs).
- **No hardware probing.** `hardware_av1` is a field the caller sets after probing; this module never inspects the device.

## Attacker exposure

Minimal. `pick_intersection` consumes two `&[VideoCodec]` slices of an already-typed enum (not raw bytes), so there is no wire-parsing surface here and no fuzz target. The attacker-touchable parsing of the *proto* capability message lives elsewhere (`lakko-proto` / `media::v1::CodecCapability` decoders); this module operates on the decoded, typed result.

## Notable

- `pick_intersection`'s doc-comment asserts `None` "should never happen because every Lakko client ships H.264 as the floor." That is a documented assumption, not an enforced invariant — nothing in this module guarantees H.264 is present in a capability list, and `default_v1()` is the only thing that seeds it. A caller that constructs a `VideoCapability` without H.264 can legitimately produce `None`. The `intersection_none_when_empty_overlap` test pins the fallback behaviour for exactly that case, so the contract is honest; just note the "cannot happen" framing is a convention upheld by callers, not by the type.

## References

- Module docs describe this as Phase 3a7 (codec choice) with SVC layer selection deferred to the quality/bandwidth sprint. No ADR is cited by the source, and none is fabricated here.
- Proto contract: `schemas/media/v1.proto` `CodecCapability.video_codecs` (the `short_name` strings).
- Sibling: `crate::codec_audio` (audio side, Opus).
- Tests: all in-module `#[cfg(test)]` — `preference_order_is_av1_vp9_h264`, `ord_matches_preference_rank`, `sdp_mime_types_match_iana_registry`, `svc_support_pinned`, the five `intersection_*` cases, the three `*_fmtp_advertises_*` cases, `default_v1_capability_carries_full_ladder`, `to_proto_strings_emits_short_names_in_order`, `short_names_match_proto_field_convention`.
