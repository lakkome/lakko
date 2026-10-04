// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import WebRTC

/// One remote tile's choice between a person's main video track and its light
/// companion (`docs/ux/group-call-grid.md` section 7.5).
///
/// The rule is not here. It is `LayerDisplay` of `lakko-ui-rules`, the one the
/// web and Android clients ask too, so the three switch at the same moments.
/// This is what stands between that rule and a tile: it stamps every report
/// with one clock, stops reporting while the app is in the background, and
/// tells the tile on the main thread when the answer changed.
///
/// Frame reports come from libwebrtc's decoder threads and ticks from the
/// main thread; `LayerDisplay` takes both at once. What this class keeps of
/// its own is under a lock.
final class RemoteLayerController {

    /// The shared rule and its counters.
    let display: LayerDisplay

    /// Called on the main thread with what the tile must show, each time it
    /// changes.
    var onShown: ((ShownLayer) -> Void)?

    private let clock: () -> UInt64
    /// Runs `work` on the main thread. Replaced in a test.
    private let onMain: (@escaping () -> Void) -> Void
    private let lock = NSLock()
    /// What the tile was last told.
    private var applied: ShownLayer = .full
    /// The app is in the background: a tile presents nothing there.
    private var suspended = false

    /// Milliseconds on a clock that never goes back.
    static func monotonicMs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000_000
    }

    init(
        lightAvailable: Bool,
        clock: @escaping () -> UInt64 = RemoteLayerController.monotonicMs,
        onMain: @escaping (@escaping () -> Void) -> Void = { work in
            if Thread.isMainThread { work() } else { DispatchQueue.main.async(execute: work) }
        }
    ) {
        self.clock = clock
        self.onMain = onMain
        self.display = LayerDisplay(nowMs: clock(), lightAvailable: lightAvailable)
    }

    /// What the tile shows.
    var shown: ShownLayer { display.shown() }

    /// The main track presented a frame. Any thread.
    func mainFrame() {
        guard !isSuspended else { return }
        publish(display.fullFrame(nowMs: clock()))
    }

    /// The light track presented a frame. Any thread.
    func lightFrame() {
        guard !isSuspended else { return }
        publish(display.lightFrame(nowMs: clock()))
    }

    /// Time has passed: a picture that stopped is noticed without a frame
    /// arriving. Called every `layerTickMs()`.
    func tick() {
        guard !isSuspended else { return }
        publish(display.tick(nowMs: clock()))
    }

    /// The person's light track appeared or went away.
    func setLightAvailable(_ available: Bool) {
        publish(display.setLightAvailable(available: available, nowMs: clock()))
    }

    /// The app went to the background: nothing is reported, so nothing is
    /// counted, until ``resume()``.
    func suspend() {
        lock.lock()
        suspended = true
        lock.unlock()
    }

    /// The app is back in the foreground: the tile starts again on the full
    /// picture, as a new tile would.
    func resume() {
        lock.lock()
        suspended = false
        lock.unlock()
        publish(display.resync(nowMs: clock()))
    }

    private var isSuspended: Bool {
        lock.lock()
        defer { lock.unlock() }
        return suspended
    }

    /// Tell the tile, once, when the answer differs from what it was told.
    private func publish(_ layer: ShownLayer) {
        lock.lock()
        let changed = layer != applied
        applied = layer
        lock.unlock()
        guard changed else { return }
        onMain { [weak self] in
            guard let self else { return }
            // Read again: a later report may have changed the answer before
            // this ran, and the tile must end on the latest one.
            self.onShown?(self.display.shown())
        }
    }
}

/// A renderer that draws nothing and reports every frame its track delivers.
///
/// libwebrtc fans each decoded frame out to every renderer attached to a
/// track, so this beside the real `RTCMTLVideoView` costs one callback per
/// frame and changes nothing about what is drawn. The callback runs on a
/// libwebrtc thread.
final class LayerFrameSink: NSObject, RTCVideoRenderer {

    private let onFrame: () -> Void

    init(onFrame: @escaping () -> Void) {
        self.onFrame = onFrame
        super.init()
    }

    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: RTCVideoFrame?) {
        guard frame != nil else { return }
        onFrame()
    }
}
