// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// The libwebrtc field trials this app turns on, and why exactly one of them.
///
/// ## The defect this exists to fix
///
/// Frame-level encryption replaces the encoded bitstream with ciphertext, and
/// libwebrtc's VP8 depacketiser classifies a frame by **reading that
/// bitstream** — `video_rtp_depacketizer_vp8.cc:180`, at the M125 revision this
/// app links (`branch-heads/6422`):
///
/// ```
/// // Read P bit from payload header (only at beginning of first partition).
/// if (video_header->is_first_packet_in_frame && (*vp8_payload & 0x01) == 0) {
///   video_header->frame_type = VideoFrameType::kVideoFrameKey;
/// ```
///
/// Under encryption `*vp8_payload` is ciphertext, so the key-frame flag is a
/// coin flip and the width and height that follow are read out of AEAD output.
/// Every frame then fails reference resolution, none completes assembly, and the
/// receiver asks for a key frame forever. Measured on a live call: 143 packets
/// and 103 KB in, `framesReceived=0`, `framesDecoded=0`, 69 dropped, 71 picture
/// requests against a sender that had encoded 17 key frames.
///
/// The frame decryptor cannot help, because it sits **downstream of assembly** —
/// `buffered_frame_decryptor.cc:39` takes an already-assembled `RtpFrameObject`,
/// and `rtp_video_stream_receiver2.cc:894` calls it only once a frame exists. On
/// that call it was never reached at all for video, while audio (whose
/// depacketiser classifies nothing from the payload) worked over the same keys.
///
/// The remedy is to carry the frame boundaries and the key-frame flag in an RTP
/// header extension, **outside** the encrypted payload, which
/// `rtp_video_stream_receiver2.cc` then uses in place of the payload read.
///
/// ## Why the generic frame descriptor and NOT the dependency descriptor
///
/// Both are real extensions and both are gated by their own trial, so the choice
/// matters. `rtp_sender_video.cc:360` writes a descriptor only when
/// `video_header.generic` is set, and then:
///
///   * the **dependency descriptor** additionally requires `video_structure_ !=
///     nullptr` (line 363). That structure is published only by codecs that
///     supply a `template_structure`, which VP8 and H.264 never do
///     (`rtp_payload_params.cc:355-363`). So for a VP8 stream the DD branch is
///     skipped and enabling `WebRTC-DependencyDescriptorAdvertised` changes
///     **nothing** — it would be a flag set in hope;
///   * the **generic frame descriptor v00** has no such requirement (line 409,
///     `IsRegistered<RtpGenericFrameDescriptorExtension00>() &&
///     !extension_is_set`), and `video_header.generic` IS populated for VP8 by
///     `Vp8ToGeneric` and for H.264 by `H264ToGeneric`
///     (`rtp_payload_params.cc:328` and `:344`).
///
/// So the generic descriptor is the one that reaches the wire for the codecs
/// this app actually negotiates, and it is the only trial enabled here.
///
/// A third trial, `WebRTC-GenericDescriptorAuth`, governs whether the descriptor
/// is covered by the SRTP header authentication. It is **already on by default**
/// — `rtp_sender_video.cc:150` reads it as `!StartsWith(lookup, "Disabled")`,
/// and an unset trial looks up to the empty string — so it needs no entry here.
/// It is named only so a reader does not go looking for a missing third flag.
///
/// ## Both ends must advertise it
///
/// An RTP header extension is negotiated, so a descriptor appears on the wire
/// only if **both** peers offer the extension. This flag on one side alone
/// changes nothing observable. The same finding applies unchanged to any other
/// libwebrtc client at this milestone: enable `WebRTC-GenericDescriptorAdvertised`
/// before the peer connection factory is built.
enum WebRTCFieldTrials {

    /// The trials, as libwebrtc's own names. Exposed rather than inlined so a
    /// test can pin the contents: the risk this guards is not that someone
    /// deletes the entry, but that someone *adds* the dependency descriptor
    /// alongside it on the reasonable-sounding belief that more is safer.
    static let dictionary: [String: String] = [
        "WebRTC-GenericDescriptorAdvertised": kRTCFieldTrialEnabledValue
    ]

    /// Install the trials. Runs exactly once per process, whichever call site
    /// gets there first.
    ///
    /// `RTCFieldTrials.h` states the constraint plainly — "Must be called before
    /// any other call into WebRTC" — so this is invoked at app launch AND as the
    /// first statement of the shared factory's lazy initialiser. The second is
    /// not belt-and-braces: the factory is the one thing every peer connection
    /// must go through, so a code path that reaches WebRTC without passing app
    /// launch still gets the trials before a factory exists.
    static func installOnce() {
        _ = installed
    }

    /// A `static let` is Swift's exactly-once, thread-safe initialiser, which is
    /// what makes ``installOnce()`` safe to call from several places: calling
    /// `RTCInitFieldTrialDictionary` twice would leave libwebrtc holding a
    /// pointer to whichever string won the race.
    private static let installed: Void = {
        RTCInitFieldTrialDictionary(dictionary)
        NSLog(
            "WebRTCFieldTrials: installed %@",
            dictionary.keys.sorted().joined(separator: ", "))
    }()
}
