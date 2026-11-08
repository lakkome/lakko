// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import CoreVideo
import LakkoCore
import WebRTC
import XCTest

@testable import Lakko

// MARK: - Fakes

/// A segmenter the test drives: what the watch answers per frame and per run.
private final class FakeSegmenter: BackgroundSegmenting {
    var frameState: BackgroundSegmentState
    var runState: BackgroundSegmentState
    var restarts = 0
    var frames = 0
    var runs = 0

    init(frame: BackgroundSegmentState, run: BackgroundSegmentState) {
        frameState = frame
        runState = run
    }

    func onFrame(nowMs: UInt64) -> BackgroundSegmentState {
        frames += 1
        return frameState
    }

    func run(bgra: [UInt8], nowMs: UInt64, mask: inout [UInt8]) -> BackgroundSegmentState {
        runs += 1
        if runState != .failed {
            for i in mask.indices { mask[i] = 255 }
        }
        return runState
    }

    func restart(nowMs: UInt64) { restarts += 1 }
}

/// A compositor that marks what it made, or fails.
private final class FakeCompositor: BackgroundCompositing {
    var fails = false
    private(set) var made: [RTCVideoFrame] = []

    func modelInput(_ frame: RTCVideoFrame) -> [UInt8]? {
        [UInt8](repeating: 0, count: BackgroundModelSize.inputBytes)
    }

    func composite(
        _ frame: RTCVideoFrame, mask: [UInt8], effect: BackgroundEffect
    ) -> RTCVideoFrame? {
        if fails { return nil }
        let out = CameraBackgroundTests.makeFrame()
        made.append(out)
        return out
    }
}

/// A camera source that records what it was handed.
private final class RecordingSource: NSObject, RTCVideoCapturerDelegate {
    private(set) var frames: [RTCVideoFrame] = []
    func capturer(_ capturer: RTCVideoCapturer, didCapture frame: RTCVideoFrame) {
        frames.append(frame)
    }
}

// MARK: - The frame gate through the fan

/// ADR-0089 section 3: while a background is selected, a frame the background
/// was not applied to never reaches a camera source, whatever goes wrong.
final class CameraBackgroundTests: XCTestCase {

    static func makeFrame() -> RTCVideoFrame {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, 32, 18, kCVPixelFormatType_32BGRA, nil, &buffer)
        return RTCVideoFrame(
            buffer: RTCCVPixelBuffer(pixelBuffer: buffer!), rotation: ._0, timeStampNs: 0)
    }

    private struct Rig {
        let stage: BackgroundFrameStage
        let fan: CapturerFan
        let main: RecordingSource
        let copy: RecordingSource
        let capturer: RTCVideoCapturer
        let queue: DispatchQueue
        let compositor: FakeCompositor
        var raw: [RTCVideoFrame] = []
        var failures = 0
    }

    /// A fan with two sources (the main one and a copy) behind a stage whose
    /// segmenter is `segmenter`, the Rust gate deciding.
    private func rig(_ segmenter: FakeSegmenter, compositor: FakeCompositor = FakeCompositor())
        -> (Rig, () -> Int)
    {
        let queue = DispatchQueue(label: "test.inference")
        var clock: UInt64 = 1000
        let stage = BackgroundFrameStage(
            makeSegmenter: { segmenter },
            compositor: compositor,
            clock: {
                clock += 100
                return clock
            },
            inferenceQueue: queue)
        var failures = 0
        stage.onFailure = { failures += 1 }
        // The call's camera is on unless a test turns it off.
        stage.setCameraSending(true)
        let main = RecordingSource()
        let copy = RecordingSource()
        let fan = CapturerFan(first: main, stage: stage)
        fan.attach(copy)
        let capturer = RTCVideoCapturer(delegate: fan)
        return (
            Rig(stage: stage, fan: fan, main: main, copy: copy, capturer: capturer,
                queue: queue, compositor: compositor),
            { failures }
        )
    }

    /// Hand `count` camera frames to the fan, letting inference finish
    /// between them. Returns the raw frames.
    private func feed(_ rig: Rig, _ count: Int) -> [RTCVideoFrame] {
        var raw: [RTCVideoFrame] = []
        for _ in 0..<count {
            let frame = Self.makeFrame()
            raw.append(frame)
            rig.fan.capturer(rig.capturer, didCapture: frame)
            rig.queue.sync {}
        }
        return raw
    }

    private func assertNoRawFrameReached(
        _ rig: Rig, raw: [RTCVideoFrame], file: StaticString = #filePath, line: UInt = #line
    ) {
        for source in [rig.main, rig.copy] {
            for frame in source.frames {
                XCTAssertFalse(
                    raw.contains { $0 === frame },
                    "an unprocessed camera frame reached a source", file: file, line: line)
            }
        }
    }

    func testAModelThatNeverLoadsSendsNothing() {
        let (rig, failures) = rig(FakeSegmenter(frame: .failed, run: .failed))
        rig.stage.setEffect(.blur(strong: false))
        let raw = feed(rig, 20)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertTrue(rig.main.frames.isEmpty)
        XCTAssertTrue(rig.copy.frames.isEmpty)
        XCTAssertEqual(failures(), 1, "the failure is reported once")
    }

    func testAModelThatNeverProducesAMaskSendsNothing() {
        let (rig, failures) = rig(FakeSegmenter(frame: .starting, run: .failed))
        rig.stage.setEffect(.builtin(id: "dawn"))
        let raw = feed(rig, 20)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertTrue(rig.main.frames.isEmpty)
        XCTAssertEqual(failures(), 0, "starting drops without failing")
    }

    func testACompositorThatFailsSendsNothingAndFails() {
        let compositor = FakeCompositor()
        compositor.fails = true
        let (rig, failures) = rig(FakeSegmenter(frame: .working, run: .working), compositor: compositor)
        rig.stage.setEffect(.blur(strong: true))
        let raw = feed(rig, 20)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertTrue(rig.main.frames.isEmpty)
        XCTAssertTrue(rig.copy.frames.isEmpty)
        XCTAssertEqual(failures(), 1)
    }

    func testAWatchThatFailsMidCallStopsEverySourceAtOnce() {
        let segmenter = FakeSegmenter(frame: .working, run: .working)
        let (rig, failures) = rig(segmenter)
        rig.stage.setEffect(.blur(strong: false))
        var raw = feed(rig, 5)
        let sentBefore = rig.main.frames.count
        segmenter.frameState = .failed
        raw += feed(rig, 10)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertEqual(rig.main.frames.count, sentBefore, "nothing after the failure")
        XCTAssertEqual(rig.copy.frames.count, sentBefore)
        XCTAssertEqual(failures(), 1)
        // Only frames the compositor made were ever handed on.
        for frame in rig.main.frames {
            XCTAssertTrue(rig.compositor.made.contains { $0 === frame })
        }
    }

    func testAWorkingBackgroundHandsOnlyProcessedFramesToEveryCopy() {
        let (rig, _) = rig(FakeSegmenter(frame: .working, run: .working))
        rig.stage.setEffect(.blur(strong: false))
        let raw = feed(rig, 10)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertFalse(rig.main.frames.isEmpty)
        XCTAssertEqual(rig.main.frames.count, rig.copy.frames.count)
        for (a, b) in zip(rig.main.frames, rig.copy.frames) {
            XCTAssertTrue(a === b, "every copy gets the same processed frame")
        }
    }

    func testNoBackgroundSendsTheCamera() {
        let (rig, _) = rig(FakeSegmenter(frame: .failed, run: .failed))
        rig.stage.setEffect(.none)
        let raw = feed(rig, 3)
        XCTAssertEqual(rig.main.frames.count, 3)
        for (sent, camera) in zip(rig.main.frames, raw) {
            XCTAssertTrue(sent === camera)
        }
    }

    func testCameraOffRunsNoModelAndSendsNothing() {
        let segmenter = FakeSegmenter(frame: .working, run: .working)
        let (rig, failures) = rig(segmenter)
        rig.stage.setEffect(.blur(strong: false))
        _ = feed(rig, 3)
        let sent = rig.main.frames.count
        let framesSeen = segmenter.frames
        let runsSeen = segmenter.runs
        let restartsSeen = segmenter.restarts

        rig.stage.setCameraSending(false)
        let raw = feed(rig, 30)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertEqual(rig.main.frames.count, sent, "nothing is handed on with the camera off")
        XCTAssertEqual(segmenter.frames, framesSeen, "the watch is not asked")
        XCTAssertEqual(segmenter.runs, runsSeen, "the model is not run")
        XCTAssertEqual(failures(), 0, "the pause is not a failure")

        rig.stage.setCameraSending(true)
        XCTAssertEqual(segmenter.restarts, restartsSeen + 1, "the watch restarts on camera on")
        _ = feed(rig, 3)
        XCTAssertGreaterThan(segmenter.frames, framesSeen)
        XCTAssertGreaterThan(rig.main.frames.count, sent)
    }

    func testAnOpenPreviewRunsTheBackgroundWithTheCameraOff() {
        let segmenter = FakeSegmenter(frame: .working, run: .working)
        let (rig, _) = rig(segmenter)
        rig.stage.setCameraSending(false)
        rig.stage.setEffect(.blur(strong: true))
        _ = feed(rig, 3)
        XCTAssertTrue(rig.main.frames.isEmpty)
        rig.stage.setPreviewOpen(true)
        let raw = feed(rig, 4)
        assertNoRawFrameReached(rig, raw: raw)
        XCTAssertFalse(rig.main.frames.isEmpty, "the preview gets processed frames")
        rig.stage.setPreviewOpen(false)
        let count = rig.main.frames.count
        _ = feed(rig, 3)
        XCTAssertEqual(rig.main.frames.count, count)
    }

    func testTheRustGate() {
        XCTAssertEqual(BackgroundGate.action(selected: false, state: .failed), .sendCamera)
        XCTAssertEqual(BackgroundGate.action(selected: true, state: .starting), .drop)
        XCTAssertEqual(BackgroundGate.action(selected: true, state: .working), .sendProcessed)
        XCTAssertEqual(BackgroundGate.action(selected: true, state: .failed), .dropAndStopCamera)
    }
}

// MARK: - The real pipeline on the simulator

/// The C ABI segmenter and the Core Image compositor on a synthetic camera
/// frame: no camera, so this proves the plumbing (model load, inference
/// through `lakko_segment_run`, the upright composite), not the picture.
final class CameraBackgroundPipelineTests: XCTestCase {

    private func cameraFrame(
        width: Int, height: Int, rotation: RTCVideoRotation, fill: Int32 = 0x80
    ) -> RTCVideoFrame {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA,
                            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &buffer)
        let pixels = buffer!
        CVPixelBufferLockBaseAddress(pixels, [])
        let base = CVPixelBufferGetBaseAddress(pixels)!
        memset(base, fill, CVPixelBufferGetBytesPerRow(pixels) * height)
        CVPixelBufferUnlockBaseAddress(pixels, [])
        return RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: pixels), rotation: rotation,
                             timeStampNs: 42)
    }

    func testTheNativeSegmenterAndCompositorProduceAnUprightFrame() throws {
        let compositor = try XCTUnwrap(CoreImageBackgroundCompositor())
        let frame = cameraFrame(width: 1280, height: 720, rotation: ._90)
        let input = try XCTUnwrap(compositor.modelInput(frame))
        XCTAssertEqual(input.count, BackgroundModelSize.inputBytes)

        let start = BackgroundClock.nowMs()
        let segmenter = NativeSegmenter(nowMs: start)
        var mask = [UInt8](repeating: 0, count: BackgroundModelSize.maskBytes)
        let state = segmenter.run(bgra: input, nowMs: start + 10, mask: &mask)
        XCTAssertEqual(state, .working)

        for effect in [BackgroundEffect.blur(strong: false), .blur(strong: true),
                       .builtin(id: "night")] {
            let out = try XCTUnwrap(compositor.composite(frame, mask: mask, effect: effect))
            XCTAssertEqual(out.rotation, ._0)
            XCTAssertEqual(out.width, 720)
            XCTAssertEqual(out.height, 1280)
            XCTAssertEqual(out.timeStampNs, 42)
        }
    }

    /// The model sees the frame's shape, never a stretched one: an upright
    /// 720x1280 frame sits in a centred 81x144 column with black bars.
    func testAnUprightFrameKeepsItsShapeInTheModelInput() throws {
        let rect = try XCTUnwrap(
            CoreImageBackgroundCompositor.modelRect(frameWidth: 720, frameHeight: 1280))
        XCTAssertEqual(rect.minX, 87.5, accuracy: 0.01)
        XCTAssertEqual(rect.width, 81, accuracy: 0.01)
        XCTAssertEqual(rect.height, 144, accuracy: 0.01)
        let map = CoreImageBackgroundCompositor.frameToModel(
            frame: CGSize(width: 720, height: 1280), rect: rect)
        XCTAssertEqual(map.a, map.d, accuracy: 1e-6, "one scale for both axes")
        let corner = CGPoint(x: 720, y: 1280).applying(map)
        XCTAssertEqual(corner.x, rect.maxX, accuracy: 0.01)
        XCTAssertEqual(corner.y, rect.maxY, accuracy: 0.01)
        let back = CGPoint(x: rect.minX, y: rect.minY).applying(map.inverted())
        XCTAssertEqual(back.x, 0, accuracy: 0.01)
        XCTAssertEqual(back.y, 0, accuracy: 0.01)

        let compositor = try XCTUnwrap(CoreImageBackgroundCompositor())
        let white = cameraFrame(width: 1280, height: 720, rotation: ._90, fill: 0xFF)
        let input = try XCTUnwrap(compositor.modelInput(white))
        func pixel(_ x: Int, _ y: Int) -> UInt8 { input[(y * BackgroundModelSize.width + x) * 4 + 1] }
        XCTAssertEqual(pixel(10, 72), 0, "a bar on the left")
        XCTAssertEqual(pixel(245, 72), 0, "a bar on the right")
        XCTAssertGreaterThan(pixel(128, 72), 200, "the frame in the middle")
    }

    func testALandscapeFrameFillsTheModelInput() throws {
        let rect = try XCTUnwrap(
            CoreImageBackgroundCompositor.modelRect(frameWidth: 1280, frameHeight: 720))
        XCTAssertEqual(rect, CGRect(x: 0, y: 0, width: 256, height: 144))
    }

    func testAWrongSizedInputFailsTheWatch() {
        let segmenter = NativeSegmenter(nowMs: BackgroundClock.nowMs())
        var mask = [UInt8](repeating: 7, count: BackgroundModelSize.maskBytes)
        XCTAssertEqual(segmenter.run(bgra: [], nowMs: BackgroundClock.nowMs(), mask: &mask), .failed)
        XCTAssertEqual(segmenter.onFrame(nowMs: BackgroundClock.nowMs()), .failed)
        XCTAssertTrue(mask.allSatisfy { $0 == 7 }, "the mask is left as it was")
    }
}

// MARK: - The stores

@MainActor
final class CameraBackgroundStoreTests: XCTestCase {
    private let account = "bg/test+account=="
    private var root: URL!
    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent("bg-tests-\(UUID().uuidString)", isDirectory: true)
        suite = "lakko.tests.background.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: root)
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    private func picture(width: Int = 40, height: Int = 30) -> CGImage {
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
        context.setFillColor(CGColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()!
    }

    func testAnUnreadableChoiceHidesTheRoom() {
        let effect = BackgroundResolution.effect(stored: "garbage:%%") { _ in nil }
        XCTAssertEqual(effect.key, "blur")
    }

    func testAnEmptyChoiceIsNone() {
        XCTAssertEqual(BackgroundResolution.effect(stored: "") { _ in nil }.key, "none")
        XCTAssertEqual(BackgroundResolution.effect(stored: "none") { _ in nil }.key, "none")
    }

    func testAMissingPhotoHidesTheRoom() {
        let store = BackgroundPhotoStore(root: root)
        let effect = BackgroundResolution.effect(stored: "photo:p1-missing") {
            store.load(id: $0, accountIdB64: self.account)
        }
        XCTAssertEqual(effect.key, "blur")
    }

    func testAStoredPhotoIsApplied() throws {
        let store = BackgroundPhotoStore(root: root)
        let photo = try store.add(picture(), accountIdB64: account, selectedId: nil)
        let effect = BackgroundResolution.effect(stored: "photo:\(photo.id)") {
            store.load(id: $0, accountIdB64: self.account)
        }
        XCTAssertEqual(effect.key, "photo:\(photo.id)")
        XCTAssertTrue(backgroundIsPhotoId(id: photo.id))
    }

    func testTheChoiceIsStoredPerAccount() {
        let choices = BackgroundChoiceStore(defaults: defaults)
        XCTAssertEqual(choices.stored(accountIdB64: account), "")
        choices.set("strong-blur", accountIdB64: account)
        XCTAssertEqual(choices.stored(accountIdB64: account), "strong-blur")
        XCTAssertEqual(choices.stored(accountIdB64: "other"), "")
        choices.clear(accountIdB64: account)
        XCTAssertEqual(choices.stored(accountIdB64: account), "")
    }

    func testAThirteenthPhotoRemovesTheOldestNotSelected() throws {
        let store = BackgroundPhotoStore(root: root)
        var added: [BackgroundPhotoStore.Photo] = []
        for i in 0..<12 {
            added.append(try store.add(
                picture(), accountIdB64: account, selectedId: nil, nowMs: UInt64(1000 + i)))
        }
        XCTAssertEqual(store.list(accountIdB64: account).count, 12)
        let selected = added[0].id
        let thirteenth = try store.add(
            picture(), accountIdB64: account, selectedId: selected, nowMs: 5000)
        let ids = store.list(accountIdB64: account).map(\.id)
        XCTAssertEqual(ids.count, BackgroundPhotoStore.limit)
        XCTAssertTrue(ids.contains(selected), "the selected photo stays")
        XCTAssertFalse(ids.contains(added[1].id), "the oldest one not selected goes")
        XCTAssertEqual(ids.first, thirteenth.id, "newest first")
        XCTAssertNil(store.load(id: added[1].id, accountIdB64: account))
    }

    func testAPhotoIsScaledToTheLongerSideLimit() throws {
        let store = BackgroundPhotoStore(root: root)
        let photo = try store.add(
            picture(width: 4000, height: 1000), accountIdB64: account, selectedId: nil)
        let loaded = try XCTUnwrap(store.load(id: photo.id, accountIdB64: account))
        XCTAssertEqual(loaded.width, BackgroundPhotoStore.maxSide)
        XCTAssertEqual(loaded.height, 480)
    }

    func testASealedAccountWritesNoPhoto() {
        let store = BackgroundPhotoStore(root: root)
        AccountStoreGate.seal(account)
        defer { AccountStoreGate.open(account) }
        XCTAssertThrowsError(try store.add(picture(), accountIdB64: account, selectedId: nil))
        XCTAssertTrue(store.list(accountIdB64: account).isEmpty)
    }

    func testTheAccountWipeRemovesThePhotosAndTheChoice() throws {
        let a = "YmFja2dyb3VuZC13aXBlLWE="
        let b = "YmFja2dyb3VuZC13aXBlLWI="
        AccountStoreGate.open(a)
        AccountStoreGate.open(b)
        let store = BackgroundPhotoStore()
        let choices = BackgroundChoiceStore()
        defer {
            try? store.delete(accountIdB64: b)
            choices.clear(accountIdB64: b)
        }
        let photoA = try store.add(picture(), accountIdB64: a, selectedId: nil)
        let photoB = try store.add(picture(), accountIdB64: b, selectedId: nil)
        choices.set("photo:\(photoA.id)", accountIdB64: a)
        choices.set("blur", accountIdB64: b)

        AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: {})

        XCTAssertTrue(store.list(accountIdB64: a).isEmpty)
        XCTAssertNil(store.load(id: photoA.id, accountIdB64: a))
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: store.directory(accountIdB64: a).path))
        XCTAssertEqual(choices.stored(accountIdB64: a), "")
        // The other account is untouched.
        XCTAssertNotNil(store.load(id: photoB.id, accountIdB64: b))
        XCTAssertEqual(choices.stored(accountIdB64: b), "blur")
        AccountStoreGate.open(a)
    }
}
