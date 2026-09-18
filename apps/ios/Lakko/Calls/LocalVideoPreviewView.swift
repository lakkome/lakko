// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import WebRTC

/// G3-1-video — SwiftUI surface for the local camera preview.
///
/// Wraps libwebrtc's `RTCMTLVideoView` (Metal-backed video
/// renderer) inside a `UIViewRepresentable` so the call UI
/// can drop the preview into any SwiftUI layout without
/// caring about UIKit lifecycle.
///
/// Renders frames from any `RTCVideoTrack` the caller hands
/// in — typically the same track that was added to the
/// `WebRTCPeerConnection` via `addLocalVideoTrack(source)`.
/// Pin the track to the **local** track if you want the
/// classic "see yourself" mirror; pin to a remote track to
/// render the other party's stream.
///
/// **Why Metal over GL:** `RTCEAGLVideoView` is deprecated in
/// iOS 12+ and removed from `stasel/WebRTC` 125. Metal is the
/// only supported path in 2026.
///
/// **Mirroring:** `videoMirrored = true` is the standard
/// posture for the front camera (the user expects a mirror
/// image, not the camera's true view). For back-cam captures
/// or remote-track renders, set `mirrored = false` at the
/// call site.
public struct LocalVideoPreviewView: UIViewRepresentable {
    /// The track to render. May be `nil` (e.g. video not yet
    /// armed); when `nil`, the underlying view renders a
    /// black frame.
    public let track: RTCVideoTrack?

    /// Mirror the rendered output horizontally. Defaults to
    /// `true` because the call surface's first use is the
    /// front-camera self-preview.
    public let mirrored: Bool

    /// Scaling mode. `.scaleAspectFill` matches the platform
    /// "video bubble" UX; `.scaleAspectFit` is what you want
    /// for picture-in-picture-style remote previews.
    public let contentMode: UIView.ContentMode

    public init(
        track: RTCVideoTrack?,
        mirrored: Bool = true,
        contentMode: UIView.ContentMode = .scaleAspectFill
    ) {
        self.track = track
        self.mirrored = mirrored
        self.contentMode = contentMode
    }

    public func makeUIView(context: Context) -> RTCMTLVideoView {
        let view = RTCMTLVideoView(frame: .zero)
        // A renderer is not a control. `UIView.isUserInteractionEnabled`
        // defaults to `true`, and a UIKit view wins hit testing against a
        // SwiftUI ancestor's gesture — so a full-screen video surface silently
        // eats every tap meant for the screen behind it. On a call that is not
        // cosmetic: the controls fade after five seconds of video, and the tap
        // that should bring them back is the one being eaten, which leaves the
        // user with no way to mute or hang up. Nothing here needs touches.
        view.isUserInteractionEnabled = false
        // `videoContentMode` is the libwebrtc-level renderer
        // setting (its enum cases are the same UIKit values).
        view.videoContentMode = contentMode
        // RTCMTLVideoView exposes `rotationOverride` (UIKit
        // transform) for mirroring; libwebrtc's preferred API
        // is `transform`, applied at the layer level — that
        // way the underlying Metal pipeline never has to
        // sample the texture twice.
        if mirrored {
            view.transform = CGAffineTransform(scaleX: -1, y: 1)
        }
        if let track = track {
            track.add(view)
            context.coordinator.attachedTrack = track
        }
        return view
    }

    public func updateUIView(_ view: RTCMTLVideoView, context: Context) {
        // Reattach if the caller swapped tracks (e.g. switched
        // from local-preview to remote-stream view).
        if context.coordinator.attachedTrack !== track {
            if let old = context.coordinator.attachedTrack {
                old.remove(view)
            }
            if let track = track {
                track.add(view)
            }
            context.coordinator.attachedTrack = track
        }
        // Re-apply mirror state — caller may have flipped it
        // (e.g. user-initiated camera switch from front to
        // back).
        view.transform = mirrored
            ? CGAffineTransform(scaleX: -1, y: 1)
            : .identity
        view.videoContentMode = contentMode
    }

    public static func dismantleUIView(_ view: RTCMTLVideoView, coordinator: Coordinator) {
        // Detach the track on tear-down so the renderer
        // releases its Metal texture pool. libwebrtc retains
        // the renderer in its renderer-list until `remove(_:)`
        // is called, so failing this step leaks one Metal
        // pool per preview-recreate cycle.
        if let track = coordinator.attachedTrack {
            track.remove(view)
            coordinator.attachedTrack = nil
        }
    }

    public func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    /// Tracks the currently-attached `RTCVideoTrack` across
    /// `updateUIView` invocations so we know whether to call
    /// `track.add(view)` / `track.remove(view)`.
    public final class Coordinator {
        fileprivate var attachedTrack: RTCVideoTrack?
    }
}
