use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

use crate::packet::MediaKind;
use crate::rtp_::Pt;
use crate::rtp_::{Direction, Frequency};

use super::codec::{Codec, CodecSpec};
use super::format_params::{FormatParams, RedParams};
use super::payload_params::{Claimed, PREFERED_RANGES, PayloadParams};

/// Default payload type for PCMU (G.711 μ-law).
pub(crate) const PT_PCMU: Pt = Pt::new_with_value(0);

/// Default payload type for PCMA (G.711 A-law).
pub(crate) const PT_PCMA: Pt = Pt::new_with_value(8);

/// Default payload type for VP8.
pub(crate) const PT_VP8: Pt = Pt::new_with_value(96);

/// Default payload type for VP8 RTX.
pub(crate) const PT_VP8_RTX: Pt = Pt::new_with_value(97);

/// Default payload type for VP9 profile 0.
pub(crate) const PT_VP9: Pt = Pt::new_with_value(98);

/// Default payload type for VP9 profile 0 RTX.
pub(crate) const PT_VP9_RTX: Pt = Pt::new_with_value(99);

/// Default payload type for AV1.
pub(crate) const PT_AV1: Pt = Pt::new_with_value(45);

/// Default payload type for AV1 RTX.
pub(crate) const PT_AV1_RTX: Pt = Pt::new_with_value(46);

/// Default payload type for H265.
pub(crate) const PT_H265: Pt = Pt::new_with_value(102);

/// Default payload type for H265 RTX.
pub(crate) const PT_H265_RTX: Pt = Pt::new_with_value(103);

/// Default payload type for Opus.
pub(crate) const PT_OPUS: Pt = Pt::new_with_value(111);

// LAKKO FORK: default payload type for audio RED (RFC 2198), the number
// libwebrtc offers.
/// Default payload type for audio RED.
pub(crate) const PT_RED: Pt = Pt::new_with_value(63);

// LAKKO FORK: default payload types for the video FEC pair, the numbers
// libwebrtc offers (`red/90000` 116 with RTX 117, `ulpfec/90000` 118).
/// Default payload type for video RED.
pub(crate) const PT_RED_VIDEO: Pt = Pt::new_with_value(116);
/// Default payload type for the RTX of video RED.
pub(crate) const PT_RED_VIDEO_RTX: Pt = Pt::new_with_value(117);
/// Default payload type for ULPFEC.
pub(crate) const PT_ULPFEC: Pt = Pt::new_with_value(118);

/// Session config for all codecs.
#[derive(Debug, Clone, Default)]
pub struct CodecConfig {
    params: Vec<PayloadParams>,
}

impl CodecConfig {
    /// Creates a new empty config.
    pub fn empty() -> Self {
        CodecConfig::default()
    }

    /// Creates a new config from payload params
    pub fn new_from_payload_params(payload_params: Vec<PayloadParams>) -> Self {
        CodecConfig {
            params: payload_params,
        }
    }

    /// Creates a new config with all default configurations enabled.
    pub fn new_with_defaults() -> Self {
        let mut c = Self::empty();
        c.enable_opus(true);

        c.enable_vp8(true);
        c.enable_h264(true);
        c.enable_h265(true);
        c.enable_vp9(true);
        c.enable_av1(true);

        c
    }

    /// Returns a reference to the payload parameters.
    pub fn params(&self) -> &[PayloadParams] {
        &self.params
    }

    /// Clear all configured configs.
    pub fn clear(&mut self) {
        self.params.clear();
    }

    /// Manually configure a payload type.
    pub fn add_config(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        codec: Codec,
        clock_rate: Frequency,
        channels: Option<u8>,
        format: FormatParams,
    ) {
        let (fb_transport_cc, fb_fir, fb_nack, fb_pli, fb_remb) = if codec.is_video() {
            (true, true, true, true, true)
        } else {
            (true, false, false, false, false)
        };

        let p = PayloadParams {
            pt,
            spec: CodecSpec {
                codec,
                clock_rate,
                channels,
                format,
            },
            resend,
            fb_transport_cc,
            fb_fir,
            fb_nack,
            fb_pli,
            fb_remb,
            locked: false,
        };

        self.params.push(p);
    }

    /// Convenience for adding a h264 payload type.
    pub fn add_h264(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        packetization_mode: bool,
        profile_level_id: u32,
    ) {
        self.add_config(
            pt,
            resend,
            Codec::H264,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                level_asymmetry_allowed: Some(true),
                packetization_mode: if packetization_mode { Some(1) } else { Some(0) },
                profile_level_id: Some(profile_level_id),
                ..Default::default()
            },
        )
    }

    /// Convenience for adding a h265 payload type.
    pub fn add_h265(
        &mut self,
        pt: Pt,
        resend: Option<Pt>,
        profile_id: u8,
        tier_flag: u8,
        level_id: u8,
    ) {
        use crate::packet::H265ProfileTierLevel;

        let ptl = H265ProfileTierLevel::new(profile_id, tier_flag, level_id)
            .unwrap_or(H265ProfileTierLevel::FALLBACK);

        self.add_config(
            pt,
            resend,
            Codec::H265,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h265_profile_tier_level: Some(ptl),
                ..Default::default()
            },
        )
    }

    /// Convenience for adding a PCM u-law payload type.
    pub fn enable_pcmu(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::PCMU);
        if !enabled {
            return;
        }
        self.add_config(
            PT_PCMU,
            None,
            Codec::PCMU,
            Frequency::EIGHT_KHZ,
            None,
            Default::default(),
        );
    }

    /// Convenience for adding a PCM a-law payload type.
    pub fn enable_pcma(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::PCMA);
        if !enabled {
            return;
        }
        self.add_config(
            PT_PCMA,
            None,
            Codec::PCMA,
            Frequency::EIGHT_KHZ,
            None,
            Default::default(),
        );
    }

    /// Add a default OPUS payload type.
    pub fn enable_opus(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Opus);
        if !enabled {
            return;
        }
        self.add_config(
            PT_OPUS,
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams {
                min_p_time: Some(10),
                use_inband_fec: Some(true),
                ..Default::default()
            },
        )
    }

    // LAKKO FORK: opt-in audio RED (RFC 2198). Not part of
    // `new_with_defaults`. The entry is placed directly before Opus because
    // the order of this list is the order of the `m=audio` format list, and
    // libwebrtc sends RED only when `red` precedes `opus` there. Its fmtp
    // names the Opus payload type twice (`111/111`), one redundant encoding.
    // Like other audio it has no RTX and no NACK. Call after `enable_opus`;
    // without an Opus entry this does nothing.
    /// Add an audio RED (RFC 2198) payload type tied to the Opus payload type.
    pub fn enable_red(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Red);
        if !enabled {
            return;
        }
        let Some(opus_idx) = self
            .params
            .iter()
            .position(|c| c.spec.codec == Codec::Opus)
        else {
            return;
        };
        let opus_pt = self.params[opus_idx].pt;
        self.add_config(
            PT_RED,
            None,
            Codec::Red,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams {
                red: Some(RedParams {
                    pt: opus_pt,
                    count: RedParams::MIN_COUNT,
                }),
                ..Default::default()
            },
        );
        // `add_config` appends; move the new entry in front of Opus.
        if let Some(red) = self.params.pop() {
            self.params.insert(opus_idx, red);
        }
    }

    // LAKKO FORK: opt-in video FEC (RED + ULPFEC, RFC 5109). Not part of
    // `new_with_defaults`. Two entries appended after the video codecs:
    // `red/90000` with an RTX payload type (a retransmitted RED packet comes
    // back under it and is delivered as the RED packet it repairs) and
    // `ulpfec/90000` without one (an FEC packet travels inside RED and is not
    // retransmitted). Neither advertises RTCP feedback, as libwebrtc's offer
    // does not. The pair is negotiated together or not at all (see
    // `MediaLine::rtp_params`). str0m never reads a RED or FEC payload.
    /// Add the video `red` and `ulpfec` payload types.
    pub fn enable_video_fec(&mut self, enabled: bool) {
        self.params
            .retain(|c| !matches!(c.spec.codec, Codec::RedVideo | Codec::Ulpfec));
        if !enabled {
            return;
        }
        for (pt, resend, codec) in [
            (PT_RED_VIDEO, Some(PT_RED_VIDEO_RTX), Codec::RedVideo),
            (PT_ULPFEC, None, Codec::Ulpfec),
        ] {
            self.add_config(
                pt,
                resend,
                codec,
                Frequency::NINETY_KHZ,
                None,
                FormatParams::default(),
            );
            if let Some(p) = self.params.last_mut() {
                p.fb_transport_cc = false;
                p.fb_fir = false;
                p.fb_nack = false;
                p.fb_pli = false;
                p.fb_remb = false;
            }
        }
    }

    /// Add a default VP8 payload type.
    pub fn enable_vp8(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Vp8);
        if !enabled {
            return;
        }
        self.add_config(
            PT_VP8,
            Some(PT_VP8_RTX),
            Codec::Vp8,
            Frequency::NINETY_KHZ,
            None,
            FormatParams::default(),
        )
    }

    /// Add a default H264 payload type.
    pub fn enable_h264(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::H264);
        if !enabled {
            return;
        }
        const PARAMS: &[(u8, u8, bool, u32)] = &[
            (127, 121, true, 0x42001f),
            (125, 107, false, 0x42001f),
            (108, 109, true, 0x42e01f),
            (124, 120, false, 0x42e01f),
            (123, 119, true, 0x4d001f),
            (35, 36, false, 0x4d001f),
            (114, 115, true, 0x64001f),
        ];

        for p in PARAMS {
            self.add_h264(p.0.into(), Some(p.1.into()), p.2, p.3)
        }
    }

    /// Add a default H265 payload type.
    /// This enables H265 as a video codec (clock rate 90kHz) with default RTX payload type.
    /// Note: H265 is still considered an experimental/hidden codec in parts of the public API,
    /// but it is supported internally for packetization/depacketization.
    pub fn enable_h265(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::H265);
        if !enabled {
            return;
        }

        // Start with single default configuration
        // Profile/tier/level negotiation will happen via SDP fmtp parameters
        //
        // Level 6.0 (level_id=180) — a high capability level well supported by modern browsers.
        // Level-id formula per ITU-T H.265 Annex A: level_id = (a*10 + b) * 3
        //   e.g. Level 6.0 → (6*10 + 0) * 3 = 180
        //
        // Chromium H265Level enum definition (kLevel6 = 180):
        //   https://source.chromium.org/chromium/chromium/src/+/main:
        //   third_party/webrtc/api/video_codecs/h265_profile_tier_level.h
        self.add_h265(PT_H265, Some(PT_H265_RTX), 1, 0, 180); // Main, Main tier, Level 6.0
    }

    /// Add a default VP9 payload type.
    pub fn enable_vp9(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Vp9);
        if !enabled {
            return;
        }
        self.add_config(
            PT_VP9,
            Some(PT_VP9_RTX),
            Codec::Vp9,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                profile_id: Some(0),
                ..Default::default()
            },
        );
        self.add_config(
            100.into(),
            Some(101.into()),
            Codec::Vp9,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                profile_id: Some(2),
                ..Default::default()
            },
        );
    }

    /// Add a default AV1 payload type.
    pub fn enable_av1(&mut self, enabled: bool) {
        self.params.retain(|c| c.spec.codec != Codec::Av1);
        if !enabled {
            return;
        }
        self.add_config(
            PT_AV1,
            Some(PT_AV1_RTX),
            Codec::Av1,
            Frequency::NINETY_KHZ,
            None,
            FormatParams::default(),
        );
    }

    /// Match the given parameters to the configured parameters.
    ///
    /// In a server scenario, a certain codec configuration might not have the same
    /// payload type (PT) for two different peers. We will have incoming data with one
    /// PT and need to match that against the PT of the outgoing.
    ///
    /// This call performs matching and if a match is found, returns the _local_ PT
    /// that can be used for sending media.
    pub fn match_params(&self, params: PayloadParams) -> Option<&PayloadParams> {
        let c = self.params.iter().max_by_key(|p| p.match_score(&params))?;
        c.match_score(&params)?; // avoid None, which isn't a match.
        Some(c)
    }

    /// When we get remote payload parameters, we need to match differently depending on direction.
    pub(crate) fn sdp_match_remote(
        &self,
        remote_params: PayloadParams,
        remote_dir: Direction,
    ) -> Option<Pt> {
        // If we have no matching parameters locally, we can't accept the remote in any way.
        let our_params = self.match_params(remote_params)?;

        if remote_dir.sdp_is_receiving() {
            // The remote is talking about its own receive requirements. The PTs are not suggestions.
            Some(remote_params.pt())
        } else {
            // We can override the remote with our local config. We would have adjusted to
            // the remote earlier if we can.
            Some(our_params.pt())
        }
    }

    /// Find a payload parameter using a finder function.
    pub fn find(&self, mut f: impl FnMut(&PayloadParams) -> bool) -> Option<&PayloadParams> {
        self.params.iter().find(move |p| f(p))
    }

    pub(crate) fn all_for_kind(&self, kind: MediaKind) -> impl Iterator<Item = &PayloadParams> {
        self.params.iter().filter(move |params| {
            if kind == MediaKind::Video {
                params.spec.codec.is_video()
            } else {
                params.spec.codec.is_audio()
            }
        })
    }

    pub(crate) fn update_params(&mut self, remote_params: &[PayloadParams], remote_dir: Direction) {
        // 0-128 of "claimed" PTs. I.e. PTs that we already allocated to something.
        let mut claimed: [bool; 128] = [false; 128];

        // Make a pass with all that are definitely confirmed by the remote, since these can't change.
        for p in self.params.iter() {
            if !p.locked {
                continue;
            }

            claimed.assert_claim_once(p.pt);

            if let Some(rtx) = p.resend {
                claimed.assert_claim_once(rtx);
            }
        }

        // Collect all currently unlocked PTs since we will need to avoid them when reassigning.
        let mut unlocked = self
            .params
            .iter()
            .filter(|p| !p.locked)
            .flat_map(|p| [Some(p.pt()), p.resend()])
            .flatten()
            .collect::<HashSet<_>>();

        // Now lock potential new parameters to remote.
        //
        // If the remote is doing `SendOnly`, we are receiving, so the PTs are suggestions,
        // and we are allowed to ANSWER with our own allocations as overrides.
        // If SendRecv or RecvOnly, the remote is talking about its own receiving capabilities
        // and we are not allowed to change it in the ANSWER.
        let local_is_controlling = !remote_dir.sdp_is_receiving();

        for p in self.params.iter_mut() {
            p.update_param(
                remote_params,
                &mut claimed,
                local_is_controlling,
                &mut unlocked,
            );
        }

        // Make a pass to reassign unconfirmed payloads that have PT which are now claimed.
        for p in self.params.iter_mut() {
            if p.locked {
                continue;
            }

            if claimed.is_claimed(p.pt) {
                let Some(pt) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
                    // TODO: handle this gracefully.
                    panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                };

                debug!("Reassigned PT {} => {}", p.pt, pt);
                p.pt = pt;

                claimed.assert_claim_once(pt);
                unlocked.remove(&pt);
            }

            let Some(rtx) = p.resend else {
                continue;
            };

            if claimed.is_claimed(rtx) {
                let Some(rtx) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
                    // TODO: handle this gracefully.
                    panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
                };

                debug!("Reassigned RTX PT {:?} => {:?}", p.resend, rtx);
                p.resend = Some(rtx);

                claimed.assert_claim_once(rtx);
                unlocked.remove(&rtx);
            }
        }
    }

    pub(crate) fn has_pt(&self, pt: Pt) -> bool {
        self.params.iter().any(|p| p.pt() == pt)
    }
}

impl Deref for CodecConfig {
    type Target = [PayloadParams];

    fn deref(&self) -> &Self::Target {
        &self.params
    }
}

impl DerefMut for CodecConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.params
    }
}

impl Codec {
    /// Get the default PT for this codec, if one exists.
    pub(crate) fn default_pt(&self) -> Option<Pt> {
        use Codec::*;
        match self {
            Opus => Some(PT_OPUS),
            PCMU => Some(PT_PCMU),
            PCMA => Some(PT_PCMA),
            Vp8 => Some(PT_VP8),
            Vp9 => Some(PT_VP9),
            H265 => Some(PT_H265),
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::packet::MediaKind;
    use crate::rtp_::Direction;
    use crate::rtp_::Frequency;

    use super::*;
    use crate::format::{CodecSpec, FormatParams};

    #[test]
    fn test_pt_conflict_different_directions() {
        // Simulates:
        // 1. str0m OFFER sendonly H264 PT 108, RTX PT 109
        // 2. FF ANSWER recvonly (locks PT 109 as H264 RTX)
        // 3. FF OFFER sendonly Opus PT 109
        // 4. Crash on PT 109 conflict

        let mut config = CodecConfig::empty();

        // Initial local config: H264 with PT 108, RTX 109 and Opus with PT 111
        config.add_config(
            108.into(),
            Some(109.into()),
            Codec::H264,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                packetization_mode: Some(1),
                profile_level_id: Some(0x42e01f),
                ..Default::default()
            },
        );
        config.add_config(
            111.into(),
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams::default(),
        );

        // Step 2: Remote answers recvonly with our H264 PT 108/109
        // This locks PT 109 as RTX
        let remote_h264_params = vec![PayloadParams::new(
            108.into(),
            Some(109.into()),
            CodecSpec {
                codec: Codec::H264,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    packetization_mode: Some(1),
                    profile_level_id: Some(0x42e01f),
                    ..Default::default()
                },
            },
        )];

        config.update_params(&remote_h264_params, Direction::RecvOnly);

        // Verify PT 109 is now locked as RTX for H264
        assert!(
            config
                .params()
                .iter()
                .any(|p| p.resend() == Some(109.into()) && p.locked)
        );

        // Step 3: Remote offers sendonly Opus PT 109
        // This should NOT crash - PT 109 should be remapped to 111 (Opus default)
        let remote_opus_params = vec![PayloadParams::new(
            109.into(),
            None,
            CodecSpec {
                codec: Codec::Opus,
                clock_rate: Frequency::FORTY_EIGHT_KHZ,
                channels: Some(2),
                format: FormatParams::default(),
            },
        )];

        config.update_params(&remote_opus_params, Direction::SendOnly);

        // Verify the fix: Opus should be remapped to PT_OPUS (its default)
        let opus_param = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus);
        assert!(opus_param.is_some(), "Opus param should exist");
        assert_eq!(
            opus_param.unwrap().pt(),
            PT_OPUS,
            "Opus should be remapped to PT_OPUS (default)"
        );
        assert!(opus_param.unwrap().locked, "Opus param should be locked");

        // Verify H264 RTX PT 109 is still locked
        assert!(
            config
                .params()
                .iter()
                .any(|p| p.resend() == Some(109.into()) && p.locked)
        );
    }

    #[test]
    fn test_pt_conflict_same_mline_multiple_codecs() {
        // Reproduces a bug we have had where reassigned PTs were conflicting with unlocked PTs.
        //
        // SCENARIO & ROOT CAUSE:
        // This test reproduces the exact bug from sdp.json using str0m's default config.
        //
        // 1. str0m starts with DEFAULT codec configuration:
        //    - VP8 at PT 96/97 (unlocked)
        //    - H264 High (0x64001f) at PT 114/115 (unlocked)
        //    - Many other H264 profiles at various PTs (127, 125, 108, 124, 123, 35)
        //
        // 2. Chrome (peer A) offers video with H264 only - PTs: 103/104, 109/114, 117/118
        //    Chrome's OFFER direction: sendonly (Chrome sends video to us)
        //    **CRITICAL**: Chrome uses PT 114 as RTX for H264 42e01f (PT 109)
        //
        // 3. str0m ANSWERs accepting Chrome's H264 offer:
        //    - Chrome's H264 PTs (103, 109, 117) are LOCKED
        //    - Chrome's RTX PTs (104, 114, 118) are LOCKED
        //    - VP8 at PT 96 remains UNLOCKED (Chrome didn't negotiate it)
        //    - H264 High at PT 114 CONFLICTS with Chrome's RTX usage of PT 114!
        //
        // 4. THE BUG (before fix): update_params reassigns H264 High from PT 114:
        //    - Searches for available PT starting from 96
        //    - PT 96 appears "unclaimed" because it only checked LOCKED PTs
        //    - VP8 is at PT 96 but UNLOCKED, so wasn't checked!
        //    - H264 High gets reassigned to PT 96
        //    - Now BOTH VP8 and H264 High are at PT 96! ❌
        //
        // 5. Later, Firefox (peer B) joins - str0m creates NEW video m-line (mid KvE)
        //    When generating SDP, str0m collects ALL video codecs from codec_config
        //    Result (before fix): PT 96 appears TWICE in the same m-line!
        //    - Line 729 in sdp.json: a=rtpmap:96 VP8/90000
        //    - Line 791 in sdp.json: a=rtpmap:96 H264/90000 profile-level-id=64001f
        //
        // EXPECTED: Each PT in an m-line must be unique
        // THE FIX: Now checks BOTH locked and unlocked PTs when reassigning

        // Start with str0m's default codec configuration
        // This includes VP8 at 96/97 and H264 High at 114/115 (among others)
        let mut config = CodecConfig::new_with_defaults();

        // Verify H264 High starts at its default PT 114 BEFORE Chrome's offer
        let h264_high_before = config
            .params()
            .iter()
            .find(|p| {
                p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f)
            })
            .expect("H264 High should exist");
        assert_eq!(
            h264_high_before.pt(),
            114.into(),
            "H264 High should start at PT 114"
        );

        // Simulate Chrome's OFFER with H264 only (sendonly from Chrome's perspective)
        // Chrome is sending video to us, so we're receiving
        // Direction::SendOnly means the remote is sending (we receive)
        let chrome_h264_params = vec![
            PayloadParams::new(
                103.into(),
                Some(104.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x42001f),
                        ..Default::default()
                    },
                },
            ),
            PayloadParams::new(
                109.into(),
                Some(114.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x42e01f),
                        ..Default::default()
                    },
                },
            ),
            PayloadParams::new(
                117.into(),
                Some(118.into()),
                CodecSpec {
                    codec: Codec::H264,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        packetization_mode: Some(1),
                        profile_level_id: Some(0x4d001f),
                        ..Default::default()
                    },
                },
            ),
        ];

        // Process Chrome's offer - this locks the H264 PTs that Chrome offered
        // Chrome is SendOnly, so from our perspective we're receiving (recvonly)
        config.update_params(&chrome_h264_params, Direction::SendOnly);

        // Verify: Chrome's H264 profiles are locked at their specific PTs
        assert!(
            config.params().iter().any(|p| p.pt() == 103.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x42001f)
                && p.locked),
            "H264 42001f should be locked at PT 103"
        );
        assert!(
            config.params().iter().any(|p| p.pt() == 109.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x42e01f)
                && p.locked),
            "H264 42e01f should be locked at PT 109"
        );
        assert!(
            config.params().iter().any(|p| p.pt() == 117.into()
                && p.spec().codec == Codec::H264
                && p.spec().format.profile_level_id == Some(0x4d001f)
                && p.locked),
            "H264 4d001f should be locked at PT 117"
        );

        // Verify: VP8 at PT 96 is NOT locked (Chrome didn't negotiate it)
        let vp8_param = config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Vp8);
        assert!(vp8_param.is_some(), "VP8 should still exist");
        assert_eq!(vp8_param.unwrap().pt(), 96.into(), "VP8 should be at PT 96");
        assert!(!vp8_param.unwrap().locked, "VP8 should NOT be locked");

        // THE FIX VERIFICATION:
        // H264 64001f was originally at PT 114, but Chrome used PT 114 for RTX
        // update_params should reassign H264 64001f to a PT that avoids BOTH:
        // - Locked PTs (103, 104, 109, 114, 117, 118)
        // - Unlocked PTs already in use (96 for VP8, 97 for VP8 RTX)
        let h264_high_param = config.params().iter().find(|p| {
            p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f)
        });
        assert!(h264_high_param.is_some(), "H264 64001f should still exist");
        let h264_pt = h264_high_param.unwrap().pt();

        // Should NOT be at PT 96 (VP8 is there)
        assert_ne!(
            h264_pt,
            96.into(),
            "FIX WORKING: H264 64001f should NOT be at PT 96 (VP8 is there)"
        );

        // Should NOT be at any of the locked PTs
        assert!(
            ![103, 104, 109, 114, 117, 118].contains(&{ *h264_pt }),
            "H264 64001f should not conflict with locked PTs"
        );

        assert!(
            !h264_high_param.unwrap().locked,
            "H264 64001f should NOT be locked (Chrome didn't offer it)"
        );

        // Now simulate creating a new video m-line (for Firefox's camera)
        // This is where the bug manifests: we collect ALL video codecs from config
        let all_video_params: Vec<_> = config.all_for_kind(MediaKind::Video).collect();

        // Extract just the PTs (including RTX PTs)
        let mut all_pts = Vec::new();
        for p in all_video_params {
            all_pts.push(p.pt());
            if let Some(rtx) = p.resend() {
                all_pts.push(rtx);
            }
        }

        // THE FIX: With the bug fixed, no PT should appear multiple times
        // Check for duplicate PTs - this should now PASS
        let mut seen = std::collections::HashSet::new();
        let mut duplicates = Vec::new();
        for pt in &all_pts {
            if !seen.insert(pt) {
                duplicates.push(*pt);
            }
        }

        assert!(
            duplicates.is_empty(),
            "FAILED: Found duplicate PTs in same m-line: {:?}\nAll PTs: {:?}\n
            This creates invalid SDP with multiple a=rtpmap lines for the same PT!",
            duplicates,
            all_pts
        );
    }

    // ── LAKKO FORK: audio RED (RFC 2198) negotiation ─────────────────────

    /// A Chrome-shaped audio offer. `{RED}` is replaced by the red lines.
    const CHROME_AUDIO_OFFER: &str = "v=0\r\n\
        o=- 5058682828002148772 2 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=group:BUNDLE 0\r\n\
        a=msid-semantic: WMS\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF {PTS} 9 0 8 13 110 126\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=rtcp:9 IN IP4 0.0.0.0\r\n\
        a=ice-ufrag:S5hk\r\n\
        a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
        a=ice-options:trickle\r\n\
        a=fingerprint:sha-256 8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:5A:BF:8B:4E:38:27:96:CA:08:49:25:73:46:60:20:DC\r\n\
        a=setup:actpass\r\n\
        a=mid:0\r\n\
        a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
        a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
        a=sendrecv\r\n\
        a=msid:- 39a7c3b1-3a23-4a1b-9c0f-6ec6a8f8d2a1\r\n\
        a=rtcp-mux\r\n\
        a=rtpmap:111 opus/48000/2\r\n\
        a=rtcp-fb:111 transport-cc\r\n\
        a=fmtp:111 minptime=10;useinbandfec=1\r\n\
        {RED}\
        a=rtpmap:9 G722/8000\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=rtpmap:8 PCMA/8000\r\n\
        a=rtpmap:13 CN/8000\r\n\
        a=rtpmap:110 telephone-event/48000\r\n\
        a=rtpmap:126 telephone-event/8000\r\n\
        a=ssrc:3948621874 cname:xeXs3aE9AOBn00yJ\r\n\
        a=ssrc:3948621874 msid:- 39a7c3b1-3a23-4a1b-9c0f-6ec6a8f8d2a1\r\n";

    fn chrome_audio_offer(pts: &str, red: &str) -> crate::change::SdpOffer {
        let sdp = CHROME_AUDIO_OFFER
            .replace("{PTS}", pts)
            .replace("{RED}", red);
        crate::change::SdpOffer::from_sdp_string(&sdp).expect("offer parses")
    }

    fn rtc_with_red() -> crate::Rtc {
        crate::init_crypto_default();
        let mut config = crate::Rtc::builder().set_rtp_mode(true);
        config.codec_config().enable_red(true);
        config.build(std::time::Instant::now())
    }

    /// The `m=audio` section of an SDP, line by line.
    fn audio_section(sdp: &str) -> Vec<String> {
        sdp.lines()
            .skip_while(|l| !l.starts_with("m=audio"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn red_fmtp_parses_and_serializes() {
        let red = RedParams::parse("111/111").expect("parses");
        assert_eq!(red.pt, 111.into());
        assert_eq!(red.count, 2);
        assert_eq!(red.to_string(), "111/111");
        assert_eq!(
            RedParams::parse("111/111/111").map(|r| r.to_string()),
            Some("111/111/111".to_owned())
        );
        // One entry, mixed payload types, more than 32 entries, not numbers,
        // not a payload type.
        assert_eq!(RedParams::parse("111"), None);
        assert_eq!(RedParams::parse("111/110"), None);
        assert_eq!(RedParams::parse(&vec!["111"; 33].join("/")), None);
        assert!(RedParams::parse(&vec!["111"; 32].join("/")).is_some());
        assert_eq!(RedParams::parse("0-15"), None);
        assert_eq!(RedParams::parse("200/200"), None);

        let line = FormatParams::parse_line("111/111");
        assert_eq!(line.red, Some(red));
        assert_eq!(line.to_string(), "111/111");
    }

    #[test]
    fn enable_red_sits_before_opus_and_has_no_rtx_or_nack() {
        let mut config = CodecConfig::new_with_defaults();
        assert!(
            !config.iter().any(|p| p.spec.codec == Codec::Red),
            "red is opt-in"
        );
        config.enable_red(true);
        let audio: Vec<_> = config.all_for_kind(MediaKind::Audio).collect();
        assert_eq!(audio.len(), 2);
        assert_eq!(audio[0].spec.codec, Codec::Red);
        assert_eq!(audio[0].pt, PT_RED);
        assert_eq!(
            audio[0].spec.format.red,
            Some(RedParams {
                pt: PT_OPUS,
                count: 2
            })
        );
        assert_eq!(audio[0].resend, None);
        assert!(!audio[0].fb_nack);
        assert_eq!(audio[1].spec.codec, Codec::Opus);

        config.enable_red(false);
        assert!(!config.iter().any(|p| p.spec.codec == Codec::Red));

        // Without Opus there is nothing to be redundant about.
        let mut config = CodecConfig::empty();
        config.enable_red(true);
        assert!(config.is_empty());
    }

    #[test]
    fn red_offer_is_answered_with_red_before_opus_and_the_same_fmtp() {
        let mut rtc = rtc_with_red();
        let offer = chrome_audio_offer(
            "111 63",
            "a=rtpmap:63 red/48000/2\r\na=fmtp:63 111/111\r\n",
        );
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let sdp = answer.to_sdp_string();
        let audio = audio_section(&sdp);
        println!("{}", audio.join("\n"));

        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 63 111");
        assert!(audio.contains(&"a=rtpmap:63 red/48000/2".to_owned()));
        assert!(audio.contains(&"a=fmtp:63 111/111".to_owned()));
        assert!(audio.contains(&"a=rtpmap:111 opus/48000/2".to_owned()));
        // Audio stays without RTX and NACK.
        assert!(!audio.iter().any(|l| l.contains("rtx")));
        assert!(!audio.iter().any(|l| l.contains("nack")));

        let mid = answer.media_lines[0].mid();
        // In the offer's order.
        assert_eq!(
            rtc.media(mid).expect("media").remote_pts(),
            &[111.into(), 63.into()]
        );
    }

    #[test]
    fn red_offer_with_other_numbers_is_answered_with_the_offerers_numbers() {
        let mut rtc = rtc_with_red();
        let offer = CHROME_AUDIO_OFFER
            .replace("{PTS}", "109 100")
            .replace(
                "{RED}",
                "a=rtpmap:100 red/48000/2\r\na=fmtp:100 109/109/109\r\n",
            )
            .replace("a=rtpmap:111 opus", "a=rtpmap:109 opus")
            .replace("a=rtcp-fb:111", "a=rtcp-fb:109")
            .replace("a=fmtp:111 minptime", "a=fmtp:109 minptime");
        let offer = crate::change::SdpOffer::from_sdp_string(&offer).expect("offer parses");
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let audio = audio_section(&answer.to_sdp_string());

        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 100 109");
        assert!(audio.contains(&"a=rtpmap:100 red/48000/2".to_owned()));
        assert!(audio.contains(&"a=fmtp:100 109/109/109".to_owned()));
        assert!(audio.contains(&"a=rtpmap:109 opus/48000/2".to_owned()));
    }

    #[test]
    fn offer_without_red_is_answered_without_red() {
        let mut rtc = rtc_with_red();
        let offer = chrome_audio_offer("111", "");
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let audio = audio_section(&answer.to_sdp_string());

        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 111");
        assert!(!audio.iter().any(|l| l.contains(" red/")));
        let mid = answer.media_lines[0].mid();
        assert_eq!(rtc.media(mid).expect("media").remote_pts(), &[111.into()]);
    }

    #[test]
    fn red_offer_naming_another_payload_type_is_answered_without_red() {
        for red in [
            // Names PCMU, not the offer's Opus.
            "a=rtpmap:63 red/48000/2\r\na=fmtp:63 0/0\r\n",
            // Names a payload type the offer does not have.
            "a=rtpmap:63 red/48000/2\r\na=fmtp:63 112/112\r\n",
            // No fmtp at all.
            "a=rtpmap:63 red/48000/2\r\n",
            // A list str0m does not represent.
            "a=rtpmap:63 red/48000/2\r\na=fmtp:63 111/0\r\n",
        ] {
            let mut rtc = rtc_with_red();
            let offer = chrome_audio_offer("111 63", red);
            let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
            let audio = audio_section(&answer.to_sdp_string());

            assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 111", "{red}");
            assert!(!audio.iter().any(|l| l.contains(" red/")), "{red}");
        }
    }

    #[test]
    fn red_is_not_answered_unless_enabled() {
        crate::init_crypto_default();
        let mut rtc = crate::Rtc::new(std::time::Instant::now());
        let offer = chrome_audio_offer(
            "111 63",
            "a=rtpmap:63 red/48000/2\r\na=fmtp:63 111/111\r\n",
        );
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let audio = audio_section(&answer.to_sdp_string());
        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 111");
    }

    #[test]
    fn own_offer_lists_red_before_opus_and_an_answer_narrows_it() {
        let mut offerer = rtc_with_red();
        let mut change = offerer.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("offer");
        let audio = audio_section(&offer.to_sdp_string());
        println!("{}", audio.join("\n"));

        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 63 111");
        assert!(audio.contains(&"a=rtpmap:63 red/48000/2".to_owned()));
        assert!(audio.contains(&"a=fmtp:63 111/111".to_owned()));

        // A peer without red answers with Opus alone; the media then has no
        // red payload type.
        crate::init_crypto_default();
        let mut plain = crate::Rtc::new(std::time::Instant::now());
        let answer = plain.sdp_api().accept_offer(offer).expect("answer");
        assert!(!answer.to_sdp_string().contains(" red/"));
        offerer
            .sdp_api()
            .accept_answer(pending, answer)
            .expect("accept");
        assert_eq!(
            offerer.media(mid).expect("media").remote_pts(),
            &[111.into()]
        );

        // A peer with red answers with both.
        let mut offerer = rtc_with_red();
        let mut change = offerer.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("offer");
        let mut with_red = rtc_with_red();
        let answer = with_red.sdp_api().accept_offer(offer).expect("answer");
        let audio = audio_section(&answer.to_sdp_string());
        assert_eq!(audio[0], "m=audio 9 UDP/TLS/RTP/SAVPF 63 111");
        offerer
            .sdp_api()
            .accept_answer(pending, answer)
            .expect("accept");
        assert_eq!(
            offerer.media(mid).expect("media").remote_pts(),
            &[63.into(), 111.into()]
        );
    }

    #[test]
    fn video_red_is_ignored() {
        // Chrome's video m-line carries `red/90000` (the ULPFEC wrapper) with
        // an RTX. It is not audio RED, and without `ulpfec` beside it, it is
        // not kept either (see the video FEC tests below).
        let line = "m=video 9 UDP/TLS/RTP/SAVPF 96 97 116 117\r\n\
            a=mid:1\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 VP8/90000\r\n\
            a=rtpmap:97 rtx/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            a=rtpmap:116 red/90000\r\n\
            a=rtpmap:117 rtx/90000\r\n\
            a=fmtp:117 apt=116\r\n";
        let sdp = CHROME_AUDIO_OFFER
            .replace("{PTS}", "111")
            .replace("{RED}", "")
            .replace("a=group:BUNDLE 0", "a=group:BUNDLE 0 1")
            + line;
        let offer = crate::change::SdpOffer::from_sdp_string(&sdp).expect("offer parses");
        let params = offer.media_lines[1].rtp_params();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].spec.codec, Codec::Vp8);
    }

    // ── LAKKO FORK: video FEC (RED + ULPFEC) negotiation ─────────────────

    /// A Chrome-shaped video offer. `{PTS}` and `{FEC}` are filled in.
    const CHROME_VIDEO_OFFER: &str = "v=0\r\n\
        o=- 5058682828002148772 2 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=group:BUNDLE 0\r\n\
        a=msid-semantic: WMS\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF {PTS}\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=rtcp:9 IN IP4 0.0.0.0\r\n\
        a=ice-ufrag:S5hk\r\n\
        a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
        a=ice-options:trickle\r\n\
        a=fingerprint:sha-256 8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:5A:BF:8B:4E:38:27:96:CA:08:49:25:73:46:60:20:DC\r\n\
        a=setup:actpass\r\n\
        a=mid:0\r\n\
        a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
        a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
        a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
        a=sendrecv\r\n\
        a=msid:- 7a9f4f0e-6f0e-4f43-8d53-1d7c3c8b2a11\r\n\
        a=rtcp-mux\r\n\
        a=rtcp-rsize\r\n\
        a=rtpmap:96 VP8/90000\r\n\
        a=rtcp-fb:96 goog-remb\r\n\
        a=rtcp-fb:96 transport-cc\r\n\
        a=rtcp-fb:96 ccm fir\r\n\
        a=rtcp-fb:96 nack\r\n\
        a=rtcp-fb:96 nack pli\r\n\
        a=rtpmap:97 rtx/90000\r\n\
        a=fmtp:97 apt=96\r\n\
        {FEC}\
        a=ssrc-group:FID 1111111111 2222222222\r\n\
        a=ssrc:1111111111 cname:xeXs3aE9AOBn00yJ\r\n\
        a=ssrc:2222222222 cname:xeXs3aE9AOBn00yJ\r\n";

    const CHROME_FEC_LINES: &str = "a=rtpmap:116 red/90000\r\n\
        a=rtpmap:117 rtx/90000\r\n\
        a=fmtp:117 apt=116\r\n\
        a=rtpmap:118 ulpfec/90000\r\n";

    fn chrome_video_offer(pts: &str, fec: &str) -> crate::change::SdpOffer {
        let sdp = CHROME_VIDEO_OFFER
            .replace("{PTS}", pts)
            .replace("{FEC}", fec);
        crate::change::SdpOffer::from_sdp_string(&sdp).expect("offer parses")
    }

    fn rtc_with_video_fec() -> crate::Rtc {
        crate::init_crypto_default();
        let mut config = crate::Rtc::builder().set_rtp_mode(true);
        config.codec_config().enable_video_fec(true);
        config.build(std::time::Instant::now())
    }

    /// The `m=video` section of an SDP, line by line.
    fn video_section(sdp: &str) -> Vec<String> {
        sdp.lines()
            .skip_while(|l| !l.starts_with("m=video"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn enable_video_fec_adds_red_with_rtx_and_ulpfec_without_feedback() {
        let mut config = CodecConfig::new_with_defaults();
        assert!(
            !config
                .iter()
                .any(|p| matches!(p.spec.codec, Codec::RedVideo | Codec::Ulpfec)),
            "video fec is opt-in"
        );
        config.enable_video_fec(true);
        let video: Vec<_> = config.all_for_kind(MediaKind::Video).collect();
        let red = video
            .iter()
            .find(|p| p.spec.codec == Codec::RedVideo)
            .expect("red");
        let fec = video
            .iter()
            .find(|p| p.spec.codec == Codec::Ulpfec)
            .expect("ulpfec");
        assert_eq!(red.pt, PT_RED_VIDEO);
        assert_eq!(red.resend, Some(PT_RED_VIDEO_RTX));
        assert_eq!(red.spec.clock_rate, Frequency::NINETY_KHZ);
        assert_eq!(fec.pt, PT_ULPFEC);
        assert_eq!(fec.resend, None);
        for p in [red, fec] {
            assert!(!p.fb_nack && !p.fb_pli && !p.fb_fir && !p.fb_remb && !p.fb_transport_cc);
        }
        // Not audio, and not confused with audio RED.
        assert!(!config
            .all_for_kind(MediaKind::Audio)
            .any(|p| matches!(p.spec.codec, Codec::RedVideo | Codec::Ulpfec)));

        config.enable_video_fec(false);
        assert!(!config
            .iter()
            .any(|p| matches!(p.spec.codec, Codec::RedVideo | Codec::Ulpfec)));
    }

    #[test]
    fn video_fec_offer_is_answered_with_both_and_the_red_rtx() {
        let mut rtc = rtc_with_video_fec();
        let offer = chrome_video_offer("96 97 116 117 118", CHROME_FEC_LINES);
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let video = video_section(&answer.to_sdp_string());
        println!("{}", video.join("\n"));

        assert_eq!(video[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97 116 117 118");
        assert!(video.contains(&"a=rtpmap:116 red/90000".to_owned()));
        assert!(video.contains(&"a=rtpmap:117 rtx/90000".to_owned()));
        assert!(video.contains(&"a=fmtp:117 apt=116".to_owned()));
        assert!(video.contains(&"a=rtpmap:118 ulpfec/90000".to_owned()));
        // The media codec keeps its own RTX and feedback; red and ulpfec
        // carry no feedback lines.
        assert!(video.contains(&"a=fmtp:97 apt=96".to_owned()));
        assert!(video.contains(&"a=rtcp-fb:96 nack".to_owned()));
        assert!(!video.iter().any(|l| l.starts_with("a=rtcp-fb:116")));
        assert!(!video.iter().any(|l| l.starts_with("a=rtcp-fb:118")));

        let mid = answer.media_lines[0].mid();
        assert_eq!(
            rtc.media(mid).expect("media").remote_pts(),
            &[96.into(), 116.into(), 118.into()]
        );
        let red = rtc
            .codec_config()
            .find(|p| p.spec.codec == Codec::RedVideo)
            .expect("red");
        assert_eq!((red.pt, red.resend), (116.into(), Some(117.into())));
    }

    #[test]
    fn video_fec_offer_with_other_numbers_is_answered_with_the_offerers_numbers() {
        let mut rtc = rtc_with_video_fec();
        let fec = "a=rtpmap:104 red/90000\r\n\
            a=rtpmap:105 rtx/90000\r\n\
            a=fmtp:105 apt=104\r\n\
            a=rtpmap:106 ulpfec/90000\r\n";
        let offer = chrome_video_offer("96 97 104 105 106", fec);
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let video = video_section(&answer.to_sdp_string());

        assert_eq!(video[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97 104 105 106");
        assert!(video.contains(&"a=rtpmap:104 red/90000".to_owned()));
        assert!(video.contains(&"a=fmtp:105 apt=104".to_owned()));
        assert!(video.contains(&"a=rtpmap:106 ulpfec/90000".to_owned()));
    }

    #[test]
    fn video_offer_with_only_one_of_red_and_ulpfec_is_answered_with_neither() {
        for (pts, fec) in [
            // red and its rtx, no ulpfec.
            (
                "96 97 116 117",
                "a=rtpmap:116 red/90000\r\na=rtpmap:117 rtx/90000\r\na=fmtp:117 apt=116\r\n",
            ),
            // ulpfec, no red.
            ("96 97 118", "a=rtpmap:118 ulpfec/90000\r\n"),
            // Neither.
            ("96 97", ""),
        ] {
            let mut rtc = rtc_with_video_fec();
            let offer = chrome_video_offer(pts, fec);
            let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
            let video = video_section(&answer.to_sdp_string());

            assert_eq!(video[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97", "{pts}");
            assert!(!video.iter().any(|l| l.contains(" red/")), "{pts}");
            assert!(!video.iter().any(|l| l.contains(" ulpfec/")), "{pts}");
            let mid = answer.media_lines[0].mid();
            assert_eq!(rtc.media(mid).expect("media").remote_pts(), &[96.into()]);
        }
    }

    #[test]
    fn video_fec_is_not_answered_unless_enabled() {
        crate::init_crypto_default();
        let mut rtc = crate::Rtc::new(std::time::Instant::now());
        let offer = chrome_video_offer("96 97 116 117 118", CHROME_FEC_LINES);
        let answer = rtc.sdp_api().accept_offer(offer).expect("answer");
        let video = video_section(&answer.to_sdp_string());
        assert_eq!(video[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97");
        assert!(!video.iter().any(|l| l.contains(" red/")));
        assert!(!video.iter().any(|l| l.contains(" ulpfec/")));
    }

    #[test]
    fn own_video_offer_lists_the_fec_pair_and_an_answer_narrows_it() {
        let mut offerer = rtc_with_video_fec();
        let mut change = offerer.sdp_api();
        let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("offer");
        let video = video_section(&offer.to_sdp_string());
        assert!(video[0].ends_with(" 116 117 118"), "{}", video[0]);
        assert!(video.contains(&"a=rtpmap:116 red/90000".to_owned()));
        assert!(video.contains(&"a=fmtp:117 apt=116".to_owned()));
        assert!(video.contains(&"a=rtpmap:118 ulpfec/90000".to_owned()));

        // A peer without the pair answers without it; the media then has
        // neither payload type.
        crate::init_crypto_default();
        let mut plain = crate::Rtc::new(std::time::Instant::now());
        let answer = plain.sdp_api().accept_offer(offer).expect("answer");
        let sdp = answer.to_sdp_string();
        assert!(!sdp.contains(" red/") && !sdp.contains(" ulpfec/"));
        offerer
            .sdp_api()
            .accept_answer(pending, answer)
            .expect("accept");
        let pts = offerer.media(mid).expect("media").remote_pts().to_vec();
        assert!(!pts.contains(&116.into()) && !pts.contains(&118.into()));

        // A peer with the pair answers with both.
        let mut offerer = rtc_with_video_fec();
        let mut change = offerer.sdp_api();
        let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("offer");
        let mut with_fec = rtc_with_video_fec();
        let answer = with_fec.sdp_api().accept_offer(offer).expect("answer");
        offerer
            .sdp_api()
            .accept_answer(pending, answer)
            .expect("accept");
        let pts = offerer.media(mid).expect("media").remote_pts().to_vec();
        assert!(pts.contains(&116.into()) && pts.contains(&118.into()));
    }

    #[test]
    fn audio_red_and_video_red_do_not_match_each_other() {
        let mut config = CodecConfig::new_with_defaults();
        config.enable_red(true);
        config.enable_video_fec(true);
        let audio = *config.find(|p| p.spec.codec == Codec::Red).expect("audio red");
        let video = *config
            .find(|p| p.spec.codec == Codec::RedVideo)
            .expect("video red");
        assert_eq!(audio.match_score(&video), None);
        assert_eq!(video.match_score(&audio), None);
        assert_eq!(config.match_params(video).map(|p| p.pt), Some(PT_RED_VIDEO));
        assert_eq!(config.match_params(audio).map(|p| p.pt), Some(PT_RED));
    }

    // ── H.265 direction-based negotiation tests ──────────────────────────

    /// Helper: build a CodecConfig containing only H.265 at the given PTL.
    fn h265_config(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecConfig {
        let mut config = CodecConfig::empty();
        config.add_h265(
            102.into(),
            Some(103.into()),
            profile_id,
            tier_flag,
            level_id,
        );
        config
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with full PTL.
    fn h265_remote_ptl(profile_id: u8, tier_flag: u8, level_id: u8) -> Vec<PayloadParams> {
        use crate::packet::H265ProfileTierLevel;
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    h265_profile_tier_level: Some(
                        H265ProfileTierLevel::new(profile_id, tier_flag, level_id).unwrap(),
                    ),
                    ..Default::default()
                },
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with profile-id only.
    fn h265_remote_profile_only(profile_id: u32) -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams {
                    profile_id: Some(profile_id),
                    ..Default::default()
                },
            },
        )]
    }

    /// Helper: build a single-element remote PayloadParams for H.265 with no fmtp.
    fn h265_remote_no_fmtp() -> Vec<PayloadParams> {
        vec![PayloadParams::new(
            102.into(),
            Some(103.into()),
            CodecSpec {
                codec: Codec::H265,
                clock_rate: Frequency::NINETY_KHZ,
                channels: None,
                format: FormatParams::default(),
            },
        )]
    }

    /// Helper: extract the H.265 param from a config (panics if missing).
    fn get_h265(config: &CodecConfig) -> &PayloadParams {
        config
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::H265)
            .expect("H.265 param missing")
    }

    // ── recvonly: remote sends to us ─────────────────────────────────────

    /// Remote sends at Level 5.2, we can receive at Level 6.0.
    /// Negotiated level should narrow to min(6.0, 5.2) = 5.2.
    #[test]
    fn test_h265_recvonly_remote_level_lower() {
        let mut config = h265_config(1, 0, 180); // Main, Main tier, Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        // Direction::SendOnly = remote is sending (we receive)
        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "Should narrow to min(180,156) = Level 5.2"
        );
        assert_eq!(ptl.profile_id(), 1, "Profile should be preserved");
        assert_eq!(ptl.tier_flag(), 0, "Tier should be preserved");
    }

    /// Remote sends at Level 6.0, we can receive at Level 5.2.
    /// Negotiated level should narrow to min(5.2, 6.0) = 5.2.
    #[test]
    fn test_h265_recvonly_local_level_lower() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

        config.update_params(&remote, Direction::SendOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "Should narrow to min(156,180) = Level 5.2"
        );
    }

    // ── sendrecv: both sides send and receive ────────────────────────────

    /// Both sides sendrecv. Local Level 6.0, remote Level 5.2.
    /// Effective level should narrow to min(6.0, 5.2) = 5.2.
    #[test]
    fn test_h265_sendrecv_remote_level_lower() {
        let mut config = h265_config(1, 0, 180); // Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
    }

    /// Both sides sendrecv. Local Level 5.2, remote Level 6.0.
    /// Effective level should narrow to min(5.2, 6.0) = 5.2.
    #[test]
    fn test_h265_sendrecv_local_level_lower() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
    }

    /// Same level on both sides. Should remain unchanged.
    #[test]
    fn test_h265_sendrecv_same_level() {
        let mut config = h265_config(1, 0, 156); // Level 5.2
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        config.update_params(&remote, Direction::SendRecv);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(ptl.level_id(), 156, "Same levels should stay at Level 5.2");
    }

    // ── sendonly: we send, remote receives ───────────────────────────────

    /// Remote declares recvonly at Level 5.2, we offer sendonly at Level 6.0.
    /// Negotiated level should narrow to 5.2 (receiver wins).
    #[test]
    fn test_h265_sendonly_receiver_level_lower() {
        let mut config = h265_config(1, 0, 180); // Level 6.0
        let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

        // Direction::RecvOnly = remote is receiving (we send)
        config.update_params(&remote, Direction::RecvOnly);

        let ptl = get_h265(&config)
            .spec()
            .format
            .h265_profile_tier_level
            .unwrap();
        assert_eq!(
            ptl.level_id(),
            156,
            "sendonly: should narrow to receiver's Level 5.2"
        );
    }

    /// Remote offers only profile-id=1, no tier/level.
    /// Local should drop its full PTL and echo back profile-id only.
    #[test]
    fn test_h265_profile_only_remote_mirrors() {
        let mut config = h265_config(1, 0, 180); // Full PTL
        let remote = h265_remote_profile_only(1); // profile-id only

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.h265_profile_tier_level.is_none(),
            "Full PTL should be cleared when remote only offers profile-id"
        );
        assert_eq!(
            h265.spec().format.profile_id,
            Some(1),
            "profile_id should mirror the remote's value"
        );
    }

    // ── no-fmtp remote ──────────────────────────────────────────────────

    /// Remote has no H.265 fmtp at all. Local should clear both fields.
    #[test]
    fn test_h265_no_fmtp_remote_clears() {
        let mut config = h265_config(1, 0, 180); // Full PTL
        let remote = h265_remote_no_fmtp();

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.h265_profile_tier_level.is_none(),
            "PTL should be cleared when remote has no fmtp"
        );
        assert!(
            h265.spec().format.profile_id.is_none(),
            "profile_id should be cleared when remote has no fmtp"
        );
    }

    // ── profile_id field cleanup ─────────────────────────────────────────

    /// When remote has full PTL, the `profile_id` field must be None
    /// to avoid duplicate serialization (profile-id appears in both
    /// h265_profile_tier_level AND the VP9-style profile_id field).
    #[test]
    fn test_h265_profile_id_cleared_on_ptl_match() {
        let mut config = CodecConfig::empty();
        // Artificially set both fields to simulate a misconfigured state
        config.add_config(
            102.into(),
            Some(103.into()),
            Codec::H265,
            Frequency::NINETY_KHZ,
            None,
            FormatParams {
                h265_profile_tier_level: Some(
                    crate::packet::H265ProfileTierLevel::new(1, 0, 180).unwrap(),
                ),
                profile_id: Some(1), // should be cleared after negotiation
                ..Default::default()
            },
        );
        let remote = h265_remote_ptl(1, 0, 156);

        config.update_params(&remote, Direction::SendRecv);

        let h265 = get_h265(&config);
        assert!(
            h265.spec().format.profile_id.is_none(),
            "profile_id field must be None when full PTL is present"
        );
        assert!(
            h265.spec().format.h265_profile_tier_level.is_some(),
            "PTL should still be present"
        );
    }

    // ── default level ────────────────────────────────────────────────────

    /// `enable_h265` should produce Level 6.0 (level_id = 180).
    #[test]
    fn test_h265_default_level_is_6_0() {
        let mut config = CodecConfig::empty();
        config.enable_h265(true);

        let h265 = get_h265(&config);
        let ptl = h265.spec().format.h265_profile_tier_level.unwrap();
        assert_eq!(
            ptl.level_id(),
            180,
            "Default H.265 level should be 6.0 (180)"
        );
        assert_eq!(
            ptl.profile_id(),
            1,
            "Default H.265 profile should be Main (1)"
        );
        assert_eq!(ptl.tier_flag(), 0, "Default H.265 tier should be Main (0)");
    }
}
