// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Opus audio codec configuration (Phase 3a6).
//!
//! Pure-data layer the platform-side WebRTC pod consults when
//! it composes the SDP offer / answer. The actual codec
//! pipeline (encode / decode) lives in each platform's native
//! WebRTC stack — Lakko's shared core only owns the **codec
//! parameter contract** that determines which Opus tunables go
//! into the SDP `a=fmtp:` line.
//!
//! ## Why this lives here, not in the platform layer
//!
//! Three platforms (web / iOS / Android) all have to agree on
//! the exact `useinbandfec`, `usedtx`, `maxaveragebitrate`,
//! `ptime`, channel count values they advertise — a drift
//! between them surfaces as a one-way audio glitch (the SDP
//! offer / answer negotiation picks the LOWER of the two
//! sides, so a platform that under-advertises caps the call
//! quality silently). Pinning the values + the SDP `fmtp`
//! string layout in shared Rust + giving every platform the
//! same builder function removes that drift surface.
//!
//! ## Profiles
//!
//! Two named profiles cover the V1 scope:
//!
//! - [`OpusProfile::Voice`] — 32 kbps mono, DTX on (silence
//!   suppression saves bandwidth on quiet ends of a call),
//!   FEC on (in-band forward error correction recovers
//!   dropped packets at <5% loss without retransmit), 20ms
//!   frames (`ptime=20`). Tuned for human speech in 1×1 /
//!   small-group calls — the dominant audio surface today.
//!
//! - [`OpusProfile::Fullband`] — 64 kbps stereo, DTX on,
//!   FEC on, 20ms frames. Reserved for the audio-rooms
//!   sprint (Phase 4); allocates the bandwidth for music
//!   pass-through or higher-fidelity speech.
//!
//! ## Bitrate adaptation
//!
//! The profile's `target_kbps` is the **ceiling** the SDP
//! advertises. The runtime [`crate::quality`] sampler can
//! drop the active bitrate below this ceiling (down to
//! [`OPUS_MIN_BITRATE_KBPS`]) on network degradation; the SDP
//! ceiling stays fixed so the codec doesn't have to
//! re-negotiate mid-call. The receiver decoder always opens
//! the lower-bandwidth stream correctly because Opus is
//! self-describing per packet.
//!
//! ## Cross-platform contract
//!
//! [`OpusConfig::sdp_fmtp_string`] is the authoritative
//! `a=fmtp:` payload — every platform calls this rather than
//! hand-rolling the string. A property test pins each
//! profile's exact output so a future tweak to one parameter
//! surfaces in CI before it lands as one-way audio in
//! production.

/// Minimum Opus bitrate the codec layer is allowed to drop
/// to under adaptive control. Below this point speech
/// intelligibility starts to degrade noticeably. Mirror of
/// [`crate::quality::OPUS_MIN_BITRATE_BPS`] / 1000.
pub const OPUS_MIN_BITRATE_KBPS: u32 = crate::quality::OPUS_MIN_BITRATE_BPS / 1000;

/// Maximum Opus bitrate the codec layer ever advertises in
/// SDP. RFC 6716 allows 510 kbps Opus payloads, but WebRTC
/// implementations in practice cap voice at ~128 kbps; the
/// quality sampler's ceiling stays at the RFC max
/// ([`crate::quality::OPUS_MAX_BITRATE_BPS`]) for math, but
/// the SDP-advertised ceiling stays at the practical
/// limit so an over-eager UA can't request 510 kbps and
/// hammer the SFU forwarding path.
pub const OPUS_MAX_BITRATE_KBPS: u32 = 128;

/// Opus codec config — pure data, no live encoder reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusConfig {
    /// Ceiling bitrate in kilobits per second. The active
    /// bitrate may drop below this under adaptive control;
    /// the SDP ceiling itself stays fixed.
    pub target_kbps: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u8,
    /// Packet time in milliseconds. 20 ms is the WebRTC
    /// default and what every browser implementation expects.
    pub ptime_ms: u8,
    /// In-band FEC enabled. Recovers <5% packet loss without
    /// re-transmit.
    pub fec: bool,
    /// Discontinuous transmission enabled. Silence
    /// suppression saves bandwidth on quiet ends of a call.
    pub dtx: bool,
}

/// Named codec profiles. The wire surface always carries an
/// explicit [`OpusConfig`]; the profiles are convenience
/// constructors for the V1 call topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusProfile {
    /// 32 kbps mono, DTX + FEC on, 20 ms frames. Tuned for
    /// speech in 1×1 / small-group calls.
    Voice,
    /// 64 kbps stereo, DTX + FEC on, 20 ms frames. Reserved
    /// for audio rooms / music pass-through (Phase 4).
    Fullband,
}

impl OpusProfile {
    /// Resolve the profile to its concrete [`OpusConfig`].
    #[must_use]
    pub const fn config(self) -> OpusConfig {
        match self {
            Self::Voice => OpusConfig {
                target_kbps: 32,
                channels: 1,
                ptime_ms: 20,
                fec: true,
                dtx: true,
            },
            Self::Fullband => OpusConfig {
                target_kbps: 64,
                channels: 2,
                ptime_ms: 20,
                fec: true,
                dtx: true,
            },
        }
    }
}

impl OpusConfig {
    /// SDP `a=fmtp:<pt> ...` payload for this Opus config.
    ///
    /// The output is the **value** part — the caller prepends
    /// `a=fmtp:<dynamic-payload-type> ` itself, because the
    /// payload-type number is negotiated per-SDP-offer and
    /// not known at this layer.
    ///
    /// Order of parameters matches what Chromium / Firefox /
    /// Safari all emit by convention: `minptime`,
    /// `useinbandfec`, `usedtx`, `maxaveragebitrate`,
    /// `stereo`, `sprop-stereo`. A receiver that parses in
    /// any order treats this verbatim, but matching the
    /// convention keeps grep / log scanning intuitive.
    #[must_use]
    pub fn sdp_fmtp_string(&self) -> String {
        let bps = u64::from(self.target_kbps) * 1000;
        let stereo_flag = u8::from(self.channels > 1);
        format!(
            "minptime={ptime};useinbandfec={fec};usedtx={dtx};maxaveragebitrate={bps};stereo={st};sprop-stereo={st}",
            ptime = self.ptime_ms,
            fec = u8::from(self.fec),
            dtx = u8::from(self.dtx),
            bps = bps,
            st = stereo_flag,
        )
    }

    /// Clamp `requested_kbps` into the supported range
    /// `[OPUS_MIN_BITRATE_KBPS .. OPUS_MAX_BITRATE_KBPS]`.
    /// Used by the quality sampler when it recommends a
    /// bitrate drop on degraded network.
    #[must_use]
    pub fn clamp_bitrate_kbps(requested_kbps: u32) -> u32 {
        requested_kbps.clamp(OPUS_MIN_BITRATE_KBPS, OPUS_MAX_BITRATE_KBPS)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_profile_is_mono_dtx_fec() {
        let cfg = OpusProfile::Voice.config();
        assert_eq!(cfg.channels, 1);
        assert_eq!(cfg.target_kbps, 32);
        assert_eq!(cfg.ptime_ms, 20);
        assert!(cfg.fec);
        assert!(cfg.dtx);
    }

    #[test]
    fn fullband_profile_is_stereo_dtx_fec() {
        let cfg = OpusProfile::Fullband.config();
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.target_kbps, 64);
        assert_eq!(cfg.ptime_ms, 20);
        assert!(cfg.fec);
        assert!(cfg.dtx);
    }

    #[test]
    fn voice_sdp_fmtp_string_matches_pinned_layout() {
        // Pinned wire-format string. Any drift surfaces here.
        // Three platforms (web / iOS / Android) advertise this
        // verbatim in their SDP offer; the receiver picks the
        // LOWER of the two sides, so a drift causes silent
        // quality degradation.
        assert_eq!(
            OpusProfile::Voice.config().sdp_fmtp_string(),
            "minptime=20;useinbandfec=1;usedtx=1;maxaveragebitrate=32000;stereo=0;sprop-stereo=0",
        );
    }

    #[test]
    fn fullband_sdp_fmtp_string_matches_pinned_layout() {
        assert_eq!(
            OpusProfile::Fullband.config().sdp_fmtp_string(),
            "minptime=20;useinbandfec=1;usedtx=1;maxaveragebitrate=64000;stereo=1;sprop-stereo=1",
        );
    }

    #[test]
    fn clamp_bitrate_into_supported_range() {
        assert_eq!(OpusConfig::clamp_bitrate_kbps(4), OPUS_MIN_BITRATE_KBPS);
        assert_eq!(OpusConfig::clamp_bitrate_kbps(200), OPUS_MAX_BITRATE_KBPS);
        assert_eq!(OpusConfig::clamp_bitrate_kbps(32), 32);
    }

    #[test]
    fn min_max_bounds_are_documented_constants() {
        // Pin the constants the public API exposes so a future
        // value change doesn't slip past review.
        assert_eq!(OPUS_MIN_BITRATE_KBPS, 8);
        assert_eq!(OPUS_MAX_BITRATE_KBPS, 128);
    }

    #[test]
    fn fmtp_string_uses_bps_not_kbps() {
        // SDP `maxaveragebitrate` is in **bits per second**,
        // not kbps. A drift to kbps here would have the
        // codec advertise 32 bps and produce silence.
        let s = OpusProfile::Voice.config().sdp_fmtp_string();
        assert!(
            s.contains("maxaveragebitrate=32000"),
            "expected bps not kbps in fmtp string, got: {s}",
        );
        assert!(
            !s.contains("maxaveragebitrate=32;"),
            "must NOT emit kbps for maxaveragebitrate: {s}",
        );
    }
}
