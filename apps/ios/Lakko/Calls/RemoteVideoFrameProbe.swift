// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// Reports the moment a remote video track actually produces a picture.
///
/// ## Why a track handle is not a picture
///
/// `RTCPeerConnectionDelegate.didAdd rtpReceiver:` fires when the **SDP** is
/// applied, not when media flows: the instant a peer's renegotiation offer adds
/// an `m=video` line, libwebrtc creates a receiver and hands over a track
/// object. So a non-nil `remoteVideoTrack` means "the other side says it will
/// send video", which is a different claim from "there is something to look at",
/// and the gap between them is however long codec negotiation, the first
/// keyframe, and — on this stack — SFrame decryption take. It can also be
/// forever, if frames arrive and cannot be opened.
///
/// A live two-device call found that gap the hard way: iOS switched to the
/// full-screen video layout on the track handle, showed a dark surface for over
/// a minute, and — because the video layout fades its controls — left the user
/// staring at a black screen with no way to mute or hang up. Keying the layout
/// on this probe instead means the portrait stays up until there is a picture,
/// which is both honest and, since a portrait never hides its controls, safe.
///
/// The probe is a renderer that draws nothing. libwebrtc fans every frame out to
/// every renderer attached to a track, so adding this alongside the real
/// `RTCMTLVideoView` costs one ignored callback per frame and changes nothing
/// about what is displayed.
final class RemoteVideoFrameProbe: NSObject, RTCVideoRenderer {

    /// Called once, on the main actor, for the first frame this track renders.
    private let onFirstFrame: () -> Void
    /// Frames arrive on a libwebrtc worker thread, so the one-shot latch is
    /// taken under a lock rather than assuming the main actor.
    private let lock = NSLock()
    private var fired = false

    init(onFirstFrame: @escaping () -> Void) {
        self.onFirstFrame = onFirstFrame
        super.init()
    }

    /// Part of `RTCVideoRenderer`; a probe has no surface to size.
    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: RTCVideoFrame?) {
        guard frame != nil else { return }
        lock.lock()
        let already = fired
        fired = true
        lock.unlock()
        guard !already else { return }
        let notify = onFirstFrame
        Task { @MainActor in notify() }
    }
}
