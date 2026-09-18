// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import WebRTC

/// The libwebrtc field trials this app turns on.
///
/// What is worth pinning here is not that the entry exists — a missing
/// descriptor shows up immediately as a call with no picture — but **which**
/// entry, and that a second one does not creep in beside it. Enabling the
/// dependency descriptor alongside the generic one is the reasonable-sounding
/// change a future reader is most likely to make, and it would be inert for
/// every codec this app negotiates while reading as though it were the fix.
final class WebRTCFieldTrialsTests: XCTestCase {

    /// The generic frame descriptor is the one that reaches the wire for VP8 and
    /// H.264: `rtp_sender_video.cc:409` writes it whenever `video_header.generic`
    /// is set, and `rtp_payload_params.cc` sets that for both codecs
    /// (`Vp8ToGeneric`, `H264ToGeneric`).
    func testTheGenericFrameDescriptorIsEnabled() {
        XCTAssertEqual(
            WebRTCFieldTrials.dictionary["WebRTC-GenericDescriptorAdvertised"],
            kRTCFieldTrialEnabledValue)
    }

    /// The dependency descriptor is INERT for the codecs this app uses, so
    /// enabling it would be a flag set in hope. `rtp_sender_video.cc:363`
    /// requires `video_structure_ != nullptr`, and that structure comes only
    /// from a codec supplying a `template_structure`, which VP8 and H.264 never
    /// do (`rtp_payload_params.cc:355-363`).
    func testTheDependencyDescriptorIsNotEnabled() {
        XCTAssertNil(
            WebRTCFieldTrials.dictionary["WebRTC-DependencyDescriptorAdvertised"],
            "inert for VP8 and H.264 — enabling it would look like a fix and do nothing")
    }

    /// `WebRTC-GenericDescriptorAuth` is already on by default —
    /// `rtp_sender_video.cc:150` reads it as `!StartsWith(lookup, "Disabled")`
    /// and an unset trial looks up to the empty string. Setting it would be
    /// noise that implies the default is otherwise.
    func testTheAuthTrialIsLeftAtItsDefault() {
        XCTAssertNil(WebRTCFieldTrials.dictionary["WebRTC-GenericDescriptorAuth"])
    }

    /// One entry, so the intent cannot be read as "turn on the descriptors".
    func testExactlyOneTrialIsSet() {
        XCTAssertEqual(WebRTCFieldTrials.dictionary.count, 1)
    }

    /// libwebrtc accepts exactly one value for an enabled trial; anything else
    /// is silently not-enabled.
    func testEveryValueIsTheEnabledSentinel() {
        for (key, value) in WebRTCFieldTrials.dictionary {
            XCTAssertEqual(value, kRTCFieldTrialEnabledValue, "trial \(key)")
        }
    }

    /// Installing is idempotent: `RTCInitFieldTrialDictionary` leaves libwebrtc
    /// holding a pointer to the string it was given, so running it twice would
    /// race two of them. Calling repeatedly must be safe, because both app
    /// launch and the shared factory call it.
    func testInstallingIsIdempotent() {
        WebRTCFieldTrials.installOnce()
        WebRTCFieldTrials.installOnce()
        WebRTCFieldTrials.installOnce()
    }
}
