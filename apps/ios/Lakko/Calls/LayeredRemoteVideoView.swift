// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit
import WebRTC

/// A remote person's picture in an SFU call: the heavy track the tile watches
/// and, when their device sends one, the light companion, in one box
/// (`docs/ux/group-call-grid.md` sections 7.5 and 7.6).
///
/// The heavy track is the person's main track or their middle one, whichever
/// the tile shows (``VideoGiven/heavyTrack(wanted:given:main:middle:hasLight:)``),
/// or none when the tile shows the light picture alone. Without a companion
/// this is one `RTCMTLVideoView` on the heavy track, as a remote tile has
/// always been. With one there are two, the same frame and the same scaling,
/// each attached to its own track for as long as the tile lives, and the one
/// ``RemoteLayerController`` names is in front.
///
/// When the heavy track is replaced by another (a tile that changed size, a
/// middle track arriving), the heavy view keeps the frame it last drew until
/// the new track's first frame; if that takes long enough for the shared rule
/// to call the picture standing, the light one comes to the front meanwhile.
/// With no heavy track the light one is in front whatever the rule says.
///
/// ## Why stacking order, not alpha and not one renderer fed from two tracks
///
/// The switch must be a hard cut that needs nothing from the track being
/// switched to. Two views that both keep drawing give that: the light view
/// already holds the light track's latest frame when it comes to the front.
/// One view fed from whichever track is chosen would show the stalled main
/// frame until the next light frame arrived, unless this file also kept the
/// last frame of the hidden track and re-sent its size, which is a second
/// renderer written by hand. Alpha would work as well as order; order leaves
/// both views fully opaque, so nothing about how a transparent Metal layer is
/// composited is being relied on. Neither view is ever hidden or removed by a
/// switch.
///
/// One type for every SFU remote picture, companion or not, so a companion
/// that arrives after the main track adds a view beside the main one instead
/// of replacing the tile's renderer.
///
/// Never mirrored: the viewer wants the other person's true orientation.
struct LayeredRemoteVideoView: UIViewRepresentable {
    /// The heavy picture the tile watches: the person's main or middle
    /// track. `nil` when the tile shows the light picture alone.
    let heavy: RTCVideoTrack?
    /// Their light companion, when the publisher sends one.
    let light: RTCVideoTrack?
    /// The SFU peer id, for the `shown` line of the call's media log.
    let peerId: String
    var contentMode: UIView.ContentMode = .scaleAspectFill

    func makeUIView(context: Context) -> LayeredRemoteVideoUIView {
        let view = LayeredRemoteVideoUIView(peerId: peerId, contentMode: contentMode)
        view.setTracks(heavy: heavy, light: light)
        return view
    }

    func updateUIView(_ view: LayeredRemoteVideoUIView, context: Context) {
        view.setContentMode(contentMode)
        view.setTracks(heavy: heavy, light: light)
    }

    static func dismantleUIView(_ view: LayeredRemoteVideoUIView, coordinator: Void) {
        view.teardown()
    }
}

/// The UIKit side of ``LayeredRemoteVideoView``.
final class LayeredRemoteVideoUIView: UIView {

    private let controller: RemoteLayerController
    private let mainView = RTCMTLVideoView(frame: .zero)
    /// Exists for as long as the person has a companion track.
    private var lightView: RTCMTLVideoView?

    private var mainTrack: RTCVideoTrack?
    private var lightTrack: RTCVideoTrack?
    private var mainSink: LayerFrameSink?
    private var lightSink: LayerFrameSink?

    private var timer: Timer?
    private var ledgerToken: Int?
    private var observers: [NSObjectProtocol] = []
    private var videoContentMode: UIView.ContentMode

    init(peerId: String, contentMode: UIView.ContentMode) {
        self.controller = RemoteLayerController(lightAvailable: false)
        self.videoContentMode = contentMode
        super.init(frame: .zero)
        // A renderer is not a control: a UIKit view wins hit testing against
        // a SwiftUI ancestor's gesture, and nothing here needs touches (see
        // `LocalVideoPreviewView`).
        isUserInteractionEnabled = false
        isAccessibilityElement = false
        clipsToBounds = true
        configure(mainView)
        addSubview(mainView)

        controller.onShown = { [weak self] layer in self?.apply(layer) }
        ledgerToken = LayerDisplayLedger.shared.register(peerId: peerId, source: controller.display)

        // A background app presents nothing; the tile starts again on the
        // full picture when it returns.
        if UIApplication.shared.applicationState == .background { controller.suspend() }
        let center = NotificationCenter.default
        observers.append(center.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.controller.suspend() })
        observers.append(center.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.controller.resume() })

        let tick = Timer(
            timeInterval: TimeInterval(layerTickMs()) / 1000, repeats: true
        ) { [weak self] _ in self?.controller.tick() }
        // `.common`, so a finger turning the grid's page does not stop it.
        RunLoop.main.add(tick, forMode: .common)
        timer = tick
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        return nil
    }

    deinit {
        timer?.invalidate()
        observers.forEach(NotificationCenter.default.removeObserver)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        mainView.frame = bounds
        lightView?.frame = bounds
    }

    func setContentMode(_ mode: UIView.ContentMode) {
        guard mode != videoContentMode else { return }
        videoContentMode = mode
        mainView.videoContentMode = mode
        lightView?.videoContentMode = mode
    }

    /// Attach the tracks. A track that did not change is left alone, so its
    /// renderer keeps the picture it has.
    func setTracks(heavy: RTCVideoTrack?, light: RTCVideoTrack?) {
        var changed = false
        if mainTrack !== heavy {
            detachMain()
            if let heavy {
                let sink = LayerFrameSink { [controller] in controller.mainFrame() }
                heavy.add(mainView)
                heavy.add(sink)
                mainTrack = heavy
                mainSink = sink
            }
            changed = true
        }
        if lightTrack !== light {
            detachLight()
            if let light {
                let view = lightView ?? makeLightView()
                let sink = LayerFrameSink { [controller] in controller.lightFrame() }
                light.add(view)
                light.add(sink)
                lightTrack = light
                lightSink = sink
            } else {
                lightView?.removeFromSuperview()
                lightView = nil
            }
            controller.setLightAvailable(light != nil)
            changed = true
        }
        if changed { apply(controller.shown) }
    }

    /// The tile is going away: detach everything and hand the counters over.
    func teardown() {
        timer?.invalidate()
        timer = nil
        observers.forEach(NotificationCenter.default.removeObserver)
        observers.removeAll()
        controller.onShown = nil
        // Close the count at the moment the tile stopped showing.
        controller.tick()
        if let token = ledgerToken {
            LayerDisplayLedger.shared.unregister(token)
            ledgerToken = nil
        }
        detachMain()
        detachLight()
    }

    // MARK: - Private

    private func configure(_ view: RTCMTLVideoView) {
        view.isUserInteractionEnabled = false
        view.videoContentMode = videoContentMode
        view.frame = bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    }

    /// The companion's view, placed behind the main one until the rule names
    /// it.
    private func makeLightView() -> RTCMTLVideoView {
        let view = RTCMTLVideoView(frame: bounds)
        configure(view)
        insertSubview(view, belowSubview: mainView)
        lightView = view
        return view
    }

    private func detachMain() {
        if let track = mainTrack {
            track.remove(mainView)
            if let sink = mainSink { track.remove(sink) }
        }
        mainTrack = nil
        mainSink = nil
    }

    private func detachLight() {
        if let track = lightTrack {
            if let view = lightView { track.remove(view) }
            if let sink = lightSink { track.remove(sink) }
        }
        lightTrack = nil
        lightSink = nil
    }

    /// Put the named picture in front. A hard cut: no fade, no badge. With
    /// no heavy track there is only the light picture to show.
    private func apply(_ layer: ShownLayer) {
        if layer == .light || mainTrack == nil, let lightView {
            bringSubviewToFront(lightView)
        } else {
            bringSubviewToFront(mainView)
        }
    }
}
