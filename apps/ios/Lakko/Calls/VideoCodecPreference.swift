// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Negotiate VP8 for a call's video (the iOS twin of web
/// `video-codec-preference.ts`).
///
/// A sealed media frame (ADR-0071) is opaque to libwebrtc's RTP packetizer:
/// the frame encryptor runs BEFORE packetization, and the packetizer still
/// works by codec type. Only VP8 has a rule for what stays in the clear. The
/// H264 packetizer looks for NAL start codes inside the frame, finds none in a
/// sealed one and sends next to nothing: measured in WebKit (113 encoded
/// frames, 0 packets sent) and in Chromium with a scrambling transform (51
/// frames, 15 packets, against 194 packets for 57 VP8 frames). Both run the
/// same libwebrtc this app links.
///
/// Nothing on iOS chose the codec: `RTCDefaultVideoEncoderFactory` lists H264
/// first, so a one-to-one call whose offer came from this device very likely
/// landed on H264. The group call never met this because the SFU offers only
/// VP8. So every video transceiver is told to prefer VP8 and the formats that
/// wrap it (retransmission, redundancy, FEC), before every local description.
/// Audio transceivers are untouched.
///
/// Not yet observed on a device: the fix rests on the browser measurements
/// above, which share this libwebrtc's packetizers.
enum VideoCodecPreference {
    /// The formats that wrap a media payload rather than carry one, as
    /// lower-cased MIME types.
    static let wrapperMimeTypes: Set<String> = [
        "video/rtx", "video/red", "video/ulpfec", "video/flexfec-03",
    ]

    /// VP8 first, then the wrappers, each in the order `capabilities` lists
    /// them; every other codec (H264, H265, VP9, AV1, ...) dropped. `nil` when
    /// `capabilities` holds no VP8. Pure; generic so a test can drive it with
    /// plain values (`RTCRtpCodecCapability` cannot be built with a chosen
    /// MIME type).
    static func vp8Preferences<C>(
        _ capabilities: [C],
        mimeType: (C) -> String
    ) -> [C]? {
        let vp8 = capabilities.filter { mimeType($0).lowercased() == "video/vp8" }
        if vp8.isEmpty { return nil }
        let wrappers = capabilities.filter { wrapperMimeTypes.contains(mimeType($0).lowercased()) }
        return vp8 + wrappers
    }

    /// The line logged once per peer connection when some video transceiver
    /// could not be told to prefer VP8. Plain text, no identifiers.
    static func unsetWarning(count: Int) -> String {
        "\(count) video transceiver(s) could not be set to VP8; "
            + "this device's video may not reach the other person"
    }
}
