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
/// ## A heavy track that was just bound shows nothing until it plays
///
/// `RTCMTLVideoView` cannot be cleared (`renderFrame(nil)` draws nothing), so
/// the heavy view holds the last frame it drew, from whatever track it was on,
/// for as long as it lives. After each bind of a heavy track (a tile that
/// changed size, a middle track arriving, the tile asking for its main or
/// middle picture again after showing the light one alone, possibly minutes
/// later) the heavy view is therefore kept out of sight until the first frame
/// that binding delivers (``HeavyBindingGate``): behind the light view when
/// there is one, transparent when there is not. The web client does the same
/// (`BindingFrameGate`, `apps/web/src/calls/video-frame-watch.ts`).
/// libwebrtc does not replay a track's last frame to a newly added renderer,
/// so the first frame a binding's sink reports was decoded after the bind.
///
/// A replaced heavy track also starts the shared rule again, as for a new tile
/// (``RemoteLayerController/resync()``), as the web and Android clients do.
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
/// composited is being relied on. Neither view is hidden or removed by a
/// switch; the heavy view's alpha changes only around a bind, and only when
/// there is no light view to put in front of it.
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
    /// Whether the heavy view may be seen for the track bound to it now.
    private var heavyGate = HeavyBindingGate()
    /// Whether ``setTracks(heavy:light:)`` ran before: the first tracks of a
    /// new tile replace nothing.
    private var attachedOnce = false
    /// What the tile shows now: read by tests.
    private(set) var arrangement = LayeredPictureArrangement(
        shown: .full, heavyVisible: false, hasLight: false)
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
        if mainTrack !== heavy || !attachedOnce {
            detachMain()
            let generation = heavyGate.bind(hasTrack: heavy != nil)
            if let heavy {
                // Only the binding's first frame crosses to the main thread;
                // every frame goes to the rule.
                let first = FirstFrameLatch()
                let sink = LayerFrameSink { [weak self, controller] in
                    controller.mainFrame()
                    guard first.take() else { return }
                    DispatchQueue.main.async { self?.heavyFirstFrame(generation) }
                }
                heavy.add(mainView)
                heavy.add(sink)
                mainTrack = heavy
                mainSink = sink
            }
            // A heavy picture that takes the place of another starts the rule
            // again, as on web and Android.
            if attachedOnce { controller.resync() }
            changed = true
        }
        attachedOnce = true
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

    /// The heavy binding `generation` delivered its first frame. It is shown
    /// a little later, so the view has drawn that frame by then and not the
    /// one it held from before the bind.
    private func heavyFirstFrame(_ generation: UInt64) {
        guard heavyGate.firstFrame(generation: generation) else { return }
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .milliseconds(Int(HeavyBindingGate.revealDelayMs))
        ) { [weak self] in
            guard let self, self.heavyGate.reveal(generation: generation) else { return }
            self.apply(self.controller.shown)
        }
    }

    /// Put the named picture in front. A hard cut: no fade, no badge. With
    /// no heavy track, or one that has not played since it was bound, there
    /// is only the light picture to show.
    private func apply(_ layer: ShownLayer) {
        let arrangement = LayeredPictureArrangement(
            shown: layer, heavyVisible: heavyGate.visible, hasLight: lightView != nil)
        self.arrangement = arrangement
        mainView.alpha = arrangement.heavyVisible ? 1 : 0
        if arrangement.front == .light, let lightView {
            bringSubviewToFront(lightView)
        } else {
            bringSubviewToFront(mainView)
        }
    }
}

/// Whether a remote tile's heavy view may be seen for the track bound to it:
/// not from the bind until shortly after the first frame that binding
/// delivered (`docs/ux/group-call-grid.md` section 7.5; the web client's
/// `BindingFrameGate`). Before that the view holds a frame drawn before the
/// bind, from this track or another one. Main thread only.
struct HeavyBindingGate {

    /// How long after a binding's first frame reaches the main thread the
    /// heavy view is shown. `RTCMTLVideoView` stores a frame on the decoder
    /// thread and draws it on its own draw cycle, so a view shown at once
    /// could still present the frame it held; this covers more than one draw
    /// cycle at 30 per second. Chosen from that reading, not measured.
    static let revealDelayMs: UInt64 = 40

    /// Counts binds; a report from an earlier binding names an older one.
    private(set) var generation: UInt64 = 0
    private var bound = false
    private var firstFrameSeen = false
    private var revealed = false

    /// A track was bound to the heavy view, or none (`hasTrack` false).
    /// Returns the binding's generation, for its frame reports.
    mutating func bind(hasTrack: Bool) -> UInt64 {
        generation &+= 1
        bound = hasTrack
        firstFrameSeen = false
        revealed = false
        return generation
    }

    /// The binding `generation` delivered a frame. `true` when it is the
    /// current binding's first, so the caller schedules ``reveal(generation:)``.
    mutating func firstFrame(generation: UInt64) -> Bool {
        guard bound, generation == self.generation, !firstFrameSeen else { return false }
        firstFrameSeen = true
        return true
    }

    /// The delay after the first frame has passed. `true` when this made the
    /// heavy view visible.
    mutating func reveal(generation: UInt64) -> Bool {
        guard bound, generation == self.generation, firstFrameSeen, !revealed else {
            return false
        }
        revealed = true
        return true
    }

    /// The heavy view may be seen.
    var visible: Bool { bound && revealed }
}

/// What a remote tile puts in front and whether its heavy view may be seen.
struct LayeredPictureArrangement: Equatable {

    enum Front: Equatable {
        case heavy
        case light
    }

    let front: Front
    let heavyVisible: Bool

    /// `shown` is what the shared rule names; `heavyVisible` what
    /// ``HeavyBindingGate`` allows; `hasLight` whether there is a light view.
    init(shown: ShownLayer, heavyVisible: Bool, hasLight: Bool) {
        self.heavyVisible = heavyVisible
        self.front = hasLight && (shown == .light || !heavyVisible) ? .light : .heavy
    }
}

/// Lets one call through, from any thread.
final class FirstFrameLatch: @unchecked Sendable {
    private let lock = NSLock()
    private var taken = false

    /// `true` the first time only.
    func take() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if taken { return false }
        taken = true
        return true
    }
}
