// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// libwebrtc's own logging, turned up far enough to answer one open question.
///
/// ## This is TEMPORARY and is meant to be deleted
///
/// It is not a feature and should not ship. libwebrtc at info level is noisy and
/// writes on media threads, so leaving it on costs console volume and some CPU
/// on every call for the benefit of a question that will be answered once.
/// ``isEnabled`` is the single switch: flip it to `false` (or delete this file
/// and its two call sites) the moment the run below has been read.
///
/// ## The question
///
/// A remote video stream arrives — 116 packets, 64 KB — and **no frame ever
/// completes**, while `first frame reached decrypt: label=video` has never
/// printed once in any configuration of this investigation, in any run, even
/// though the audio line prints every time in the same call.
///
/// Reading `branch-heads/6422` narrowed that to two gates, and both announce
/// themselves at exactly `LS_INFO`, which iOS suppresses by default. One run
/// with this on distinguishes them:
///
///   * **`webrtc_video_engine.cc:3595`** — `"Setting FrameDecryptor (recv)
///     because of SetFrameDecryptor, remote_ssrc="`. Emitted only inside
///     `if (stream_)`. Its presence says the decryptor reached the receive
///     stream, and names the SSRC it landed on; its ABSENCE says our
///     `setFrameDecryptor:` got as far as the media channel's config and no
///     further, because no stream existed yet.
///
///   * **`buffered_frame_decryptor.cc:62`** — `"Frame decryption required but
///     not attached to this stream. Stashing frame."`. Its presence says frames
///     ARE completing assembly and are being held — `FrameDecision::kStash` —
///     because the stream has no decryptor. That would explain every
///     observation at once: assembled, never decrypted, never reference-
///     resolved (so `framesReceived` stays 0), and never dropped.
///
/// If neither line appears, the stash hypothesis is refuted and the frames are
/// stopping earlier, in `PacketBuffer`, which is a different search.
///
/// ## What the comparison already ruled out
///
/// `VideoRtpReceiver::SetFrameDecryptor` (`pc/video_rtp_receiver.cc:85-93`) and
/// `AudioRtpReceiver::SetFrameDecryptor` (`pc/audio_rtp_receiver.cc:146-154`)
/// are **byte-identical**, down to the comment:
///
/// ```
///   frame_decryptor_ = std::move(frame_decryptor);
///   // Special Case: Set the frame decryptor to any value on any existing channel.
///   if (media_channel_ && signaled_ssrc_) {
///     media_channel_->SetFrameDecryptor(*signaled_ssrc_, frame_decryptor_);
///   }
/// ```
///
/// So the audio-works / video-does-not asymmetry is NOT in that guard, and both
/// kinds re-apply the decryptor when their SSRC is later set up (video `:163`,
/// audio `:298`). Whatever differs is downstream of there or is a matter of
/// ordering, which is precisely what these two log lines report.
enum WebRTCDiagnosticLogging {

    /// The switch. `false` restores the framework's default silence.
    ///
    /// Deliberately a plain constant rather than a build configuration: this is
    /// scaffolding for one investigation, and a reader deleting it should not
    /// have to chase a flag through the project file.
    static let isEnabled = true

    /// Raise libwebrtc's minimum log level. Must run before the framework does
    /// anything worth logging, so it is called alongside the field trials.
    static func installOnce() {
        guard isEnabled else { return }
        _ = installed
    }

    private static let installed: Void = {
        RTCSetMinDebugLogLevel(.info)
        NSLog(
            "WebRTCDiagnosticLogging: libwebrtc log level raised to info "
                + "(TEMPORARY - remove once the video decryptor attachment is settled)")
    }()
}
