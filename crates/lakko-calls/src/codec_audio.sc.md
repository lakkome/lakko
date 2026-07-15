<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/codec_audio.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if codec_audio.rs changes and this file does not. -->

# `codec_audio` — Opus SDP parameter contract (spec)

**Module:** `crates/lakko-calls/src/codec_audio.rs`
**One line:** Own the Opus codec parameter values (bitrate / channels / ptime / FEC / DTX) and the exact `a=fmtp:` string layout that all three platforms advertise in their SDP offer/answer, so they cannot drift.

## Why this module exists

The actual Opus encode/decode pipeline lives in each platform's native WebRTC stack — Lakko's shared core does not touch a codec. What the core DOES own is the **parameter contract**: which tunables (`useinbandfec`, `usedtx`, `maxaveragebitrate`, `ptime`, channel count) each platform bakes into its SDP `a=fmtp:` line.

The load-bearing reason to centralise this: SDP negotiation resolves each codec parameter to the LOWER of the two sides' advertised values. If web, iOS, and Android hand-roll their own `fmtp` strings and one platform under-advertises, the call silently caps at the weaker side's quality — a one-way audio glitch with no error surfaced. Pinning the values and the string layout in shared Rust, with one builder function every platform calls, removes that drift surface entirely.

## Public contract

- `OpusConfig` — pure data (`Copy`), no live encoder reference. Carries `target_kbps` (the ceiling, not the live bitrate), `channels`, `ptime_ms`, `fec`, `dtx`. The wire surface always carries an explicit `OpusConfig`; profiles are only convenience constructors.
- `OpusProfile::{Voice, Fullband}` + `config()` — resolve a named profile to its concrete `OpusConfig`. `Voice` = 32 kbps mono, DTX+FEC on, 20 ms frames (the dominant 1×1 / small-group speech surface today). `Fullband` = 64 kbps stereo, DTX+FEC on, 20 ms frames (reserved for the Phase-4 audio-rooms / music-passthrough sprint). `config()` is `const`.
- `OpusConfig::sdp_fmtp_string()` — the authoritative `a=fmtp:` **value** payload. Returns the parameter list only; the caller prepends `a=fmtp:<dynamic-payload-type> ` because the payload-type number is negotiated per offer and not known at this layer. Emits parameters in the Chromium/Firefox/Safari convention order (`minptime`, `useinbandfec`, `usedtx`, `maxaveragebitrate`, `stereo`, `sprop-stereo`) — a receiver parses order-independently, but matching the convention keeps log/grep scanning intuitive.
- `OpusConfig::clamp_bitrate_kbps(requested)` — clamp a requested bitrate into `[OPUS_MIN_BITRATE_KBPS ..= OPUS_MAX_BITRATE_KBPS]`. Called by the `quality` sampler when it recommends a bitrate drop on a degraded network.
- `OPUS_MIN_BITRATE_KBPS` / `OPUS_MAX_BITRATE_KBPS` — the adaptive-control floor (8 kbps, derived from `quality::OPUS_MIN_BITRATE_BPS / 1000`) and the SDP-advertised ceiling (128 kbps).

## Behavioral guarantees

- **`maxaveragebitrate` is emitted in bits per second, not kbps.** `sdp_fmtp_string` multiplies `target_kbps` by 1000. This is not cosmetic: the SDP field is defined in bps, so emitting `32` instead of `32000` would advertise 32 bps and produce silence. Pinned by a dedicated test.
- **Each profile's fmtp string is byte-pinned.** `Voice` and `Fullband` each have a property test asserting their exact output string, so a tweak to any single parameter surfaces in CI before it ships as one-way audio.
- **The advertised bitrate is a ceiling, not the live rate.** The `quality` sampler may run the active bitrate below `target_kbps` (down to `OPUS_MIN_BITRATE_KBPS`) without re-negotiating SDP; the receiver decoder still opens the lower-bandwidth stream because Opus is self-describing per packet.
- **The SDP ceiling (128 kbps) is deliberately below the RFC 6716 max (510 kbps).** WebRTC UAs cap voice near 128 kbps in practice; keeping the SDP-advertised ceiling there prevents an over-eager UA from requesting 510 kbps and hammering the SFU forwarding path. The `quality` module's math ceiling stays at the RFC max; the two ceilings are intentionally separate.
- **`stereo` and `sprop-stereo` always carry the same flag**, derived from `channels > 1`.

## Failure modes

None. Every public function is total: `config()` and `sdp_fmtp_string()` cannot fail, and `clamp_bitrate_kbps` clamps rather than rejecting. No panics, no `Result`. The `u64::from(target_kbps) * 1000` widening in `sdp_fmtp_string` cannot overflow for any realistic profile value.

## What this module does NOT do

- **No codec pipeline.** It does not encode, decode, or hold an encoder handle — that is the platform WebRTC stack's job. This is a data-only parameter layer.
- **No SDP parsing or emission.** It produces one `fmtp` value substring; it does not build, parse, or negotiate a full SDP body, and does not know the negotiated payload-type number.
- **No live bitrate control.** The adaptive drop decision lives in `crate::quality`; this module only supplies the clamp bounds and the ceiling.

## Attacker exposure

None on the parsing side — this module has no decoder and consumes no attacker-controlled bytes. Its output is a static ASCII string composed only from compile-time-known profile fields (or a caller-supplied `u32` that is clamped, never parsed). There is no fuzz target because there is no untrusted input surface.

## References

- Sibling module `crate::quality` — owns `OPUS_MIN_BITRATE_BPS` / `OPUS_MAX_BITRATE_BPS` (the constants this module divides down) and the runtime bitrate sampler.
- Tests (in-module `#[cfg(test)]`): `voice_profile_is_mono_dtx_fec`, `fullband_profile_is_stereo_dtx_fec`, `voice_sdp_fmtp_string_matches_pinned_layout`, `fullband_sdp_fmtp_string_matches_pinned_layout`, `clamp_bitrate_into_supported_range`, `min_max_bounds_are_documented_constants`, `fmtp_string_uses_bps_not_kbps`.
