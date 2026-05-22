// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Video codec negotiation (Phase 3a7).
//!
//! Pure-data layer for choosing the video codec the
//! platform's WebRTC pod will encode under. Sibling of
//! [`crate::codec_audio`] for the audio side.
//!
//! ## Why this lives in shared Rust
//!
//! The actual codec pipeline (encode / decode) lives in
//! each platform's native WebRTC stack — but the **preference
//! list** advertised in SDP and the **intersection rule** the
//! offer/answer negotiation runs MUST agree byte-for-byte
//! across web / iOS / Android. A drift surfaces as either a
//! silent quality regression (one side advertises AV1, the
//! other only picks VP9 even though AV1 is supported by
//! both) or a hard codec mismatch (no common codec → no
//! video at all).
//!
//! Pinning the preference list + intersection rule in shared
//! Rust + giving every platform the same builder removes
//! that drift surface.
//!
//! ## Codec ladder
//!
//! Three codecs cover the V1 scope, ranked by efficiency
//! (highest preference first):
//!
//! 1. **AV1** — best compression for the same perceived
//!    quality, software-decode on every platform from 2024+,
//!    hardware-decode on Apple Silicon (M1+) and Snapdragon
//!    8 Gen 1+. ~30% bitrate saving vs. VP9 at the same VMAF.
//!    SVC (Scalable Video Coding) layers via the AV1
//!    temporal-spatial structure.
//!
//! 2. **VP9** — middle ground. Hardware-decode on every
//!    Android + Windows since 2017, software-only on iOS
//!    (no native HW until A14, AV1 isn't HW either there).
//!    SVC layers stable. ~15% bitrate saving vs. H.264.
//!    Fallback when AV1 isn't available (older Android,
//!    older browsers).
//!
//! 3. **H.264 baseline-3.1** — universal fallback. Hardware-
//!    decode on every iOS device since 2010. No SVC (only
//!    simulcast — multiple independent streams). Apple
//!    legacy: Safari pre-16.4 supports H.264 but not VP9
//!    HW-decode; certain enterprise iOS deployments still
//!    pin Safari versions. We ship H.264 as the floor so the
//!    call always has a path to video.
//!
//! The audio side has no equivalent ladder because Opus is
//! universally supported and runs on every platform's
//! hardware-decode path (or fast software-decode).
//!
//! ## Intersection rule
//!
//! Offer / answer negotiation picks the **highest-rank
//! codec both sides advertise**. If A advertises
//! `[AV1, VP9, H264]` and B advertises `[VP9, H264]`, the
//! pick is VP9. The actual SDP negotiation in the WebRTC
//! stack handles this for us; we surface
//! [`VideoCodec::pick_intersection`] to drive UI hints
//! ("you'll be talking to N who only supports H.264 →
//! expect 480p instead of 1080p") before the SDP exchange
//! completes.
//!
//! ## SVC layer selection
//!
//! Out of scope for 3a7 — that's [`crate::quality`]'s
//! bandwidth adaptation sprint (3c7). This module pins the
//! codec **choice** + the **default SVC stream descriptor**;
//! the runtime layer-switching state machine lives next door.

/// Video codecs Lakko's V1 ladder advertises. Ranked by
/// efficiency — higher discriminant = higher preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VideoCodec {
    /// Universal fallback. Every iOS + Android since ~2010
    /// hardware-decodes this. No SVC, simulcast only.
    H264 = 0,
    /// Middle ground. HW on every Android + Windows since
    /// 2017, software-only on iOS. SVC layers stable.
    Vp9 = 1,
    /// Best compression. SW on every platform from 2024+,
    /// HW on Apple Silicon M1+ + Snapdragon 8 Gen 1+. AV1
    /// SVC via temporal-spatial structure.
    Av1 = 2,
}

impl VideoCodec {
    /// Canonical preference list, highest-rank first. This
    /// is the order every platform advertises in its SDP
    /// offer; the receiver picks the highest-rank match.
    #[must_use]
    pub const fn preference_order() -> [Self; 3] {
        [Self::Av1, Self::Vp9, Self::H264]
    }

    /// SDP `mimeType` — the value WebRTC's RTCRtpSender /
    /// RTCRtpReceiver report as the codec.
    #[must_use]
    pub const fn sdp_mime_type(self) -> &'static str {
        match self {
            Self::Av1 => "video/AV1",
            Self::Vp9 => "video/VP9",
            Self::H264 => "video/H264",
        }
    }

    /// Short identifier for diagnostics / logging. Matches
    /// the conventional spelling in WebRTC tooling.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
            Self::H264 => "h264",
        }
    }

    /// Whether this codec supports Scalable Video Coding
    /// (SVC) — multiple temporal-spatial layers in a single
    /// stream, allowing the SFU to drop layers per-receiver
    /// without re-encoding.
    ///
    /// H.264 does NOT (only simulcast — multiple independent
    /// streams). Pinning this here so the platform layer
    /// doesn't have to grow a separate codec-capability
    /// table.
    #[must_use]
    pub const fn supports_svc(self) -> bool {
        match self {
            Self::Av1 | Self::Vp9 => true,
            Self::H264 => false,
        }
    }

    /// SDP `a=fmtp:` payload (the value part, without the
    /// leading `a=fmtp:<pt> ` prefix the caller prepends).
    ///
    /// Codec-specific parameters baked in:
    /// - AV1: `level-idx=5;profile=0;tier=0` — Main Profile,
    ///   Level 5.1 (covers up to 4K @ 60fps; far above what
    ///   V1 needs, but it's the WebRTC default).
    /// - VP9: `profile-id=0` — 8-bit 4:2:0, the only profile
    ///   every WebRTC implementation negotiates without
    ///   custom shaping.
    /// - H.264: `level-asymmetry-allowed=1;packetization-
    ///   mode=1;profile-level-id=42e01f` — Constrained
    ///   Baseline Level 3.1, the maximum every Safari (incl.
    ///   pre-16.4) decodes hardware-accelerated.
    #[must_use]
    pub fn sdp_fmtp_string(self) -> String {
        match self {
            Self::Av1 => "level-idx=5;profile=0;tier=0".to_string(),
            Self::Vp9 => "profile-id=0".to_string(),
            Self::H264 => {
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string()
            },
        }
    }

    /// Pick the highest-rank codec both `offer` and `answer`
    /// advertise. Returns `None` only when the intersection
    /// is empty — which in practice should never happen
    /// because every Lakko client ships H.264 as the floor.
    ///
    /// Used for **pre-SDP UI hints** ("expect 480p — N's
    /// device only supports H.264"); the actual codec
    /// selection happens inside the platform WebRTC stack
    /// from the SDP offer / answer exchange.
    #[must_use]
    pub fn pick_intersection(offer: &[Self], answer: &[Self]) -> Option<Self> {
        Self::preference_order()
            .into_iter()
            .find(|c| offer.contains(c) && answer.contains(c))
    }
}

/// Video codec capability advertised by a participant on
/// join. Mirror of the audio side's `OpusConfig` —
/// pure-data, no live encoder reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCapability {
    /// Codecs this participant supports, ordered by their
    /// preference (highest-preferred first).
    pub codecs: Vec<VideoCodec>,
    /// Whether the participant's hardware accelerates AV1
    /// decode. Informational — the SFU can use this to
    /// pick simulcast vs SVC layer routing per-receiver.
    pub hardware_av1: bool,
    /// Whether the participant supports SVC layer dropping
    /// (the codec-side answer is in
    /// `VideoCodec::supports_svc`; this field is the
    /// **runtime** answer — a participant on a CPU-bound
    /// device might disable SVC even on a codec that
    /// supports it).
    pub svc_enabled: bool,
}

impl VideoCapability {
    /// Default V1 capability — advertises every codec in
    /// the canonical order, SVC enabled, HW-AV1 unknown
    /// (caller flips after probing the platform).
    #[must_use]
    pub fn default_v1() -> Self {
        Self {
            codecs: VideoCodec::preference_order().to_vec(),
            hardware_av1: false,
            svc_enabled: true,
        }
    }

    /// Render the capability into the `video_codecs` slice
    /// of [`media::v1::CodecCapability`] — short codec
    /// names in preference order.
    #[must_use]
    pub fn to_proto_strings(&self) -> Vec<String> {
        self.codecs
            .iter()
            .map(|c| c.short_name().to_string())
            .collect()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_order_is_av1_vp9_h264() {
        // The order is the load-bearing contract: the
        // receiver picks the highest-rank codec both sides
        // advertise. Any change here cascades across all
        // three platforms.
        assert_eq!(
            VideoCodec::preference_order(),
            [VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
        );
    }

    #[test]
    fn ord_matches_preference_rank() {
        // Higher Ord = higher preference. The intersection
        // picker relies on this when the preference list
        // is iterated in declarative order.
        assert!(VideoCodec::Av1 > VideoCodec::Vp9);
        assert!(VideoCodec::Vp9 > VideoCodec::H264);
    }

    #[test]
    fn sdp_mime_types_match_iana_registry() {
        // RFC 7741 (VP9 RTP), draft-ietf-payload-vp9 (AV1
        // RTP), RFC 6184 (H.264 RTP) all pin the mime type
        // capitalisation as VP9/AV1/H264 — uppercase.
        assert_eq!(VideoCodec::Av1.sdp_mime_type(), "video/AV1");
        assert_eq!(VideoCodec::Vp9.sdp_mime_type(), "video/VP9");
        assert_eq!(VideoCodec::H264.sdp_mime_type(), "video/H264");
    }

    #[test]
    fn svc_support_pinned() {
        assert!(VideoCodec::Av1.supports_svc());
        assert!(VideoCodec::Vp9.supports_svc());
        assert!(
            !VideoCodec::H264.supports_svc(),
            "H.264 is simulcast-only — pin the negative answer so a future enum flip surfaces"
        );
    }

    // -----------------------------------------------------------
    // Intersection picker
    // -----------------------------------------------------------

    #[test]
    fn intersection_picks_av1_when_both_sides_have_full_ladder() {
        let pick = VideoCodec::pick_intersection(
            &[VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
            &[VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
        );
        assert_eq!(pick, Some(VideoCodec::Av1));
    }

    #[test]
    fn intersection_picks_vp9_when_one_side_lacks_av1() {
        let pick = VideoCodec::pick_intersection(
            &[VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
            &[VideoCodec::Vp9, VideoCodec::H264],
        );
        assert_eq!(pick, Some(VideoCodec::Vp9));
    }

    #[test]
    fn intersection_falls_back_to_h264_when_no_overlap_above() {
        let pick = VideoCodec::pick_intersection(
            &[VideoCodec::Av1, VideoCodec::H264],
            &[VideoCodec::Vp9, VideoCodec::H264],
        );
        assert_eq!(pick, Some(VideoCodec::H264));
    }

    #[test]
    fn intersection_none_when_empty_overlap() {
        // Synthetic case — in production every Lakko client
        // ships H.264 as the floor, so this branch should
        // never fire. Pin the behaviour anyway for the rare
        // future where someone removes H.264 support on a
        // platform.
        let pick = VideoCodec::pick_intersection(&[VideoCodec::Av1], &[VideoCodec::H264]);
        assert_eq!(pick, None);
    }

    #[test]
    fn intersection_order_is_independent_of_input_order() {
        // The picker iterates the canonical preference
        // order, not the order of the input slices — so
        // a quirky offer order doesn't change the answer.
        let pick = VideoCodec::pick_intersection(
            &[VideoCodec::H264, VideoCodec::Av1],
            &[VideoCodec::Vp9, VideoCodec::H264, VideoCodec::Av1],
        );
        assert_eq!(pick, Some(VideoCodec::Av1));
    }

    // -----------------------------------------------------------
    // fmtp wire strings
    // -----------------------------------------------------------

    #[test]
    fn h264_fmtp_advertises_constrained_baseline_3_1() {
        // 0x42e01f is the WebRTC-canonical Constrained
        // Baseline Level 3.1 profile-level-id — the
        // maximum every Safari pre-16.4 hardware-decodes.
        // Drift here breaks iOS-Safari interop.
        let s = VideoCodec::H264.sdp_fmtp_string();
        assert!(
            s.contains("profile-level-id=42e01f"),
            "H.264 fmtp must advertise Constrained Baseline 3.1: {s}",
        );
        assert!(
            s.contains("packetization-mode=1"),
            "H.264 fmtp must enable single-NAL-unit packetisation: {s}",
        );
    }

    #[test]
    fn av1_fmtp_advertises_main_profile_level_5() {
        let s = VideoCodec::Av1.sdp_fmtp_string();
        assert!(s.contains("profile=0"));
        assert!(s.contains("level-idx=5"));
        assert!(s.contains("tier=0"));
    }

    #[test]
    fn vp9_fmtp_advertises_profile_0() {
        let s = VideoCodec::Vp9.sdp_fmtp_string();
        assert!(s.contains("profile-id=0"));
    }

    // -----------------------------------------------------------
    // VideoCapability builder
    // -----------------------------------------------------------

    #[test]
    fn default_v1_capability_carries_full_ladder() {
        let cap = VideoCapability::default_v1();
        assert_eq!(cap.codecs, VideoCodec::preference_order());
        assert!(!cap.hardware_av1);
        assert!(cap.svc_enabled);
    }

    #[test]
    fn to_proto_strings_emits_short_names_in_order() {
        let cap = VideoCapability::default_v1();
        assert_eq!(cap.to_proto_strings(), vec!["av1", "vp9", "h264"]);
    }

    #[test]
    fn short_names_match_proto_field_convention() {
        // schemas/media/v1.proto's CodecCapability.video_codecs
        // documents these exact strings ("av1", "vp9",
        // "h264"). Pin so a casual rename doesn't desync
        // the wire format silently.
        assert_eq!(VideoCodec::Av1.short_name(), "av1");
        assert_eq!(VideoCodec::Vp9.short_name(), "vp9");
        assert_eq!(VideoCodec::H264.short_name(), "h264");
    }
}
