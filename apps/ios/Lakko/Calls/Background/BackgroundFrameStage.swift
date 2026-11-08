// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import WebRTC

/// The camera background's processing stage (ADR-0089, UX
/// `docs/ux/call-backgrounds.md`): what every camera frame passes through
/// between the capturer and the call's video sources.
///
/// ``CapturerFan`` asks the stage once per capturer frame, before it hands the
/// frame to the main source and to every copy's source (ADR-0070), so the
/// main, middle and light copies and the self view all get the same answer.
/// The 1:1 call's camera goes through the same fan. A shared screen does not
/// go through it.
protocol CameraFrameStage: AnyObject {
    /// The frame to hand to every camera source, or `nil` to send nothing
    /// for this capturer frame.
    func process(_ frame: RTCVideoFrame) -> RTCVideoFrame?
}

/// The processing state, as the Rust health watch reports it.
enum BackgroundSegmentState: Equatable, Sendable {
    /// The model is loading or has produced no mask yet.
    case starting
    /// Masks arrive in time.
    case working
    /// Final until a restart.
    case failed
}

/// What to do with one camera frame: the answer of the Rust frame gate
/// (`lakko_segment_frame_action`).
enum BackgroundFrameAction: Equatable, Sendable {
    case sendCamera
    case sendProcessed
    case drop
    case dropAndStopCamera
}

/// What the call shows about the background (UX section 3).
enum BackgroundPhase: Equatable, Sendable {
    /// No background selected.
    case off
    /// A background selected, not ready: nothing is sent.
    case starting
    /// A background selected and applied.
    case working
    /// The background stopped working: nothing is sent, the camera is turned
    /// off.
    case failed
}

/// The background the stage applies, resolved from the stored choice.
enum BackgroundEffect {
    case none
    case blur(strong: Bool)
    case builtin(id: String)
    /// An own photo, already loaded (a photo that could not be loaded never
    /// reaches here: it resolves to `blur`).
    case photo(id: String, image: CGImage)

    var isBackground: Bool {
        if case .none = self { return false }
        return true
    }

    /// A stable key, for caches and tests.
    var key: String {
        switch self {
        case .none: return "none"
        case .blur(let strong): return strong ? "strong-blur" : "blur"
        case .builtin(let id): return "builtin:\(id)"
        case .photo(let id, _): return "photo:\(id)"
        }
    }
}

/// One camera's segmenter and health watch (the C ABI `lakko_segment_*`).
protocol BackgroundSegmenting: AnyObject {
    /// A camera frame arrived at `nowMs`: the state to judge it by. Never
    /// waits for an inference.
    func onFrame(nowMs: UInt64) -> BackgroundSegmentState
    /// Segments one 256x144 BGRA picture into `mask` (256x144 bytes). An
    /// input of the wrong size fails the watch. On failure `mask` is left as
    /// it was.
    func run(bgra: [UInt8], nowMs: UInt64, mask: inout [UInt8]) -> BackgroundSegmentState
    /// Forgets the mask history and restarts the watch.
    func restart(nowMs: UInt64)
}

/// The pixel work: the model's input picture and the composite. Core Image
/// in the app (``CoreImageBackgroundCompositor``).
protocol BackgroundCompositing: AnyObject {
    /// The frame upright, scaled to 256x144, BGRA, not mirrored, rows top to
    /// bottom. `nil` when the frame cannot be read.
    func modelInput(_ frame: RTCVideoFrame) -> [UInt8]?
    /// The frame upright with `effect` behind the person `mask` marks.
    /// `nil` when it cannot be made.
    func composite(
        _ frame: RTCVideoFrame, mask: [UInt8], effect: BackgroundEffect
    ) -> RTCVideoFrame?
}

/// The size of the model's input and of its mask.
enum BackgroundModelSize {
    static let width = 256
    static let height = 144
    static var maskBytes: Int { width * height }
    static var inputBytes: Int { width * height * 4 }
}

/// The stage: asks the Rust frame gate about every frame and never hands a
/// frame on that the background was not applied to while one is selected.
///
/// ## Threading
///
/// ``process(_:)`` runs on the capturer's queue. Inference runs on a serial
/// queue of its own, at most one at a time and at most
/// ``minInferenceIntervalMs`` apart; the frames in between are composited
/// with the last mask. Selection and restarts come from the main actor. All
/// shared state is under one lock, which is never held across a call out
/// (the segmenter, the compositor, a callback).
final class BackgroundFrameStage: CameraFrameStage {

    typealias Gate = (_ selected: Bool, _ state: BackgroundSegmentState) -> BackgroundFrameAction

    /// The fastest inference rate: one every 66 ms (about 15 a second).
    /// Chosen, not measured: well inside the watch's 2 s stall window and
    /// 250 ms average, and below a 30 fps camera so a phone does not run the
    /// model on every frame.
    static let minInferenceIntervalMs: UInt64 = 66

    private let lock = NSLock()
    private var effect: BackgroundEffect = .none
    private var segmenter: BackgroundSegmenting?
    private var creatingSegmenter = false
    private var mask: [UInt8]?
    /// Bumped on every restart, so a mask computed before it is not used
    /// after it.
    private var generation: UInt64 = 0
    private var inferenceInFlight = false
    private var lastInferenceMs: UInt64?
    /// The compositor could not make a frame while the watch said Working:
    /// the stage fails on its own (fail closed) until the next restart.
    private var compositeFailed = false
    private var failureReported = false
    private var publishedPhase: BackgroundPhase?
    /// The call's camera track is on (``setCameraSending(_:)``).
    private var cameraSending = false
    /// Open previews of the processed picture (``setPreviewOpen(_:)``).
    private var previewHolds = 0

    /// Whether frames are processed at all: only while the camera is sending
    /// or a preview shows the result. Otherwise nothing could leave the
    /// device anyway, and the model is not run.
    private var processing: Bool { cameraSending || previewHolds > 0 }

    private let makeSegmenter: () -> BackgroundSegmenting
    private let compositor: BackgroundCompositing
    private let gate: Gate
    private let clock: () -> UInt64
    private let inferenceQueue: DispatchQueue

    /// The phase changed. Called on the capturer's or the caller's thread.
    var onPhase: ((BackgroundPhase) -> Void)?
    /// The background failed (once per failure). Called on the capturer's
    /// thread.
    var onFailure: (() -> Void)?
    /// Every frame the stage hands on, for the settings preview. Called on
    /// the capturer's thread.
    var onOutput: ((RTCVideoFrame) -> Void)?

    init(
        makeSegmenter: @escaping () -> BackgroundSegmenting,
        compositor: BackgroundCompositing,
        gate: @escaping Gate = { BackgroundGate.action(selected: $0, state: $1) },
        clock: @escaping () -> UInt64 = BackgroundClock.nowMs,
        inferenceQueue: DispatchQueue = DispatchQueue(
            label: "me.lakko.call-background.inference", qos: .userInitiated)
    ) {
        self.makeSegmenter = makeSegmenter
        self.compositor = compositor
        self.gate = gate
        self.clock = clock
        self.inferenceQueue = inferenceQueue
    }

    // MARK: - Selection

    /// Apply `next`. Going from no background to one restarts the watch; so
    /// does a background the person `chose` after a failure (the Rust rule:
    /// a failure is final until the person picks again). Moving between two
    /// backgrounds while it works keeps the mask, so there is no gap.
    func setEffect(_ next: BackgroundEffect, chosen: Bool = false) {
        lock.lock()
        let wasBackground = effect.isBackground
        let failed = compositeFailed || failureReported
        effect = next
        lock.unlock()
        if !next.isBackground {
            publish(.off)
            return
        }
        if !wasBackground || (chosen && failed) {
            restart()
        }
    }

    /// The effect applied now.
    var currentEffect: BackgroundEffect {
        lock.lock()
        defer { lock.unlock() }
        return effect
    }

    /// Forget the masks and restart the watch: a background chosen again,
    /// the camera switched or turned back on.
    func restart() {
        lock.lock()
        generation &+= 1
        mask = nil
        compositeFailed = false
        failureReported = false
        lastInferenceMs = nil
        let seg = segmenter
        let selected = effect.isBackground
        lock.unlock()
        seg?.restart(nowMs: clock())
        publish(selected ? .starting : .off)
        if selected { ensureSegmenter() }
    }

    // MARK: - Camera on and off

    /// The call's camera track was switched on or off (the camera button,
    /// a track added or removed). While it is off and no preview is open,
    /// frames with a background selected are dropped without running the
    /// model, so a camera-off call costs nothing for this. Coming back on
    /// restarts the watch, so the pause never reads as a stall.
    func setCameraSending(_ on: Bool) {
        lock.lock()
        let was = processing
        cameraSending = on
        let now = processing
        lock.unlock()
        if !was && now { restart() }
    }

    /// A preview of the processed picture opened or closed (the settings
    /// sheet's, which works with the camera off). Holds are counted.
    func setPreviewOpen(_ open: Bool) {
        lock.lock()
        let was = processing
        previewHolds = max(0, previewHolds + (open ? 1 : -1))
        let now = processing
        lock.unlock()
        if !was && now { restart() }
    }

    // MARK: - Per frame

    func process(_ frame: RTCVideoFrame) -> RTCVideoFrame? {
        lock.lock()
        let effect = self.effect
        let seg = segmenter
        let forcedFailure = compositeFailed
        let active = processing
        lock.unlock()

        guard effect.isBackground else {
            // No background: the camera as it is (the gate's first row).
            switch gate(false, .working) {
            case .sendCamera:
                publish(.off)
                onOutput?(frame)
                return frame
            default:
                return nil
            }
        }

        // The camera is off and nothing shows the picture: the frame goes
        // nowhere, so it is dropped without asking the model or the watch.
        guard active else { return nil }

        let now = clock()
        var state: BackgroundSegmentState
        if let seg {
            state = seg.onFrame(nowMs: now)
        } else {
            ensureSegmenter()
            state = .starting
        }
        if forcedFailure { state = .failed }

        switch gate(true, state) {
        case .sendProcessed:
            scheduleInference(frame, nowMs: now)
            lock.lock()
            let currentMask = mask
            lock.unlock()
            guard let currentMask else {
                // The watch reports the first mask a moment before this stage
                // has stored it (or a restart dropped it): nothing to apply
                // yet, so nothing is sent.
                publish(.starting)
                return nil
            }
            guard let out = compositor.composite(frame, mask: currentMask, effect: effect) else {
                // Working by the watch, but no picture can be made: fail
                // here rather than send anything else.
                lock.lock()
                compositeFailed = true
                lock.unlock()
                fail()
                return nil
            }
            publish(.working)
            onOutput?(out)
            return out
        case .drop:
            scheduleInference(frame, nowMs: now)
            publish(.starting)
            return nil
        case .dropAndStopCamera:
            fail()
            return nil
        case .sendCamera:
            // The gate never answers this with a background selected; the
            // raw camera is refused all the same.
            return nil
        }
    }

    // MARK: - Internals

    private func fail() {
        publish(.failed)
        lock.lock()
        let first = !failureReported
        failureReported = true
        lock.unlock()
        if first { onFailure?() }
    }

    private func publish(_ phase: BackgroundPhase) {
        lock.lock()
        let changed = publishedPhase != phase
        publishedPhase = phase
        lock.unlock()
        if changed { onPhase?(phase) }
    }

    /// Load the model off the capturer's thread (tens of milliseconds).
    private func ensureSegmenter() {
        lock.lock()
        guard segmenter == nil, !creatingSegmenter else {
            lock.unlock()
            return
        }
        creatingSegmenter = true
        lock.unlock()
        inferenceQueue.async { [weak self] in
            guard let self else { return }
            let made = self.makeSegmenter()
            self.lock.lock()
            self.segmenter = made
            self.creatingSegmenter = false
            self.lock.unlock()
        }
    }

    private func scheduleInference(_ frame: RTCVideoFrame, nowMs: UInt64) {
        lock.lock()
        guard let seg = segmenter, !inferenceInFlight else {
            lock.unlock()
            return
        }
        if let last = lastInferenceMs, nowMs &- last < Self.minInferenceIntervalMs {
            lock.unlock()
            return
        }
        inferenceInFlight = true
        lastInferenceMs = nowMs
        let gen = generation
        lock.unlock()

        inferenceQueue.async { [weak self] in
            guard let self else { return }
            // A frame that cannot be read is handed on as an empty picture,
            // which fails the watch: a host bug turns the camera off rather
            // than leaving it on with a stale mask.
            let input = self.compositor.modelInput(frame) ?? []
            var out = [UInt8](repeating: 0, count: BackgroundModelSize.maskBytes)
            let state = seg.run(bgra: input, nowMs: self.clock(), mask: &out)
            self.lock.lock()
            if state != .failed, gen == self.generation {
                self.mask = out
            }
            self.inferenceInFlight = false
            self.lock.unlock()
        }
    }
}

/// The monotonic clock the watch is fed, in milliseconds.
enum BackgroundClock {
    static func nowMs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000_000
    }
}
