// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import XCTest

@testable import Lakko

/// The capture post-processing stage of ADR-0088 against a fake native
/// suppressor: when it cleans, when it leaves the buffer alone, and that a
/// suppressor is never freed under the audio thread. The same cases as
/// Android's `CaptureNoiseProcessorTest`, plus the switch and the native
/// engine itself.
final class CaptureNoiseProcessorTests: XCTestCase {

    private final class FakeEngine: NSObject, LakkoNoiseEngine {
        let sampleRate: Int
        private let lock = NSLock()
        private var next = 1
        private var _created: [Int] = []
        private var _freed: [Int] = []
        private var _processed: [Int] = []
        var status: Int32 = 0
        var createFails = false
        var gate: DispatchSemaphore?
        var entered: DispatchSemaphore?

        init(sampleRate: Int = 48_000) {
            self.sampleRate = sampleRate
        }

        var created: [Int] { lock.withLock { _created } }
        var freed: [Int] { lock.withLock { _freed } }
        var processed: [Int] { lock.withLock { _processed } }

        func createSuppressor() -> UnsafeMutableRawPointer? {
            if createFails { return nil }
            return lock.withLock {
                let h = next
                next += 1
                _created.append(h)
                return UnsafeMutableRawPointer(bitPattern: h)
            }
        }

        func destroySuppressor(_ handle: UnsafeMutableRawPointer?) {
            guard let handle else { return }
            lock.withLock { _freed.append(Int(bitPattern: handle)) }
        }

        func processSuppressor(
            _ handle: UnsafeMutableRawPointer,
            samples: UnsafeMutablePointer<Float>,
            frames: Int
        ) -> Int32 {
            entered?.signal()
            _ = gate?.wait(timeout: .now() + 5)
            lock.withLock { _processed.append(Int(bitPattern: handle)) }
            if status == 0 { samples[0] = 0 }
            return status
        }
    }

    private var lines: [String] = []
    private let linesLock = NSLock()

    private func processor(_ engine: FakeEngine) -> LakkoCaptureNoiseProcessor {
        LakkoCaptureNoiseProcessor(
            engine: engine,
            log: { [weak self] line in
                guard let self else { return }
                self.linesLock.withLock { self.lines.append(line) }
            },
            pause: { usleep(1000) })
    }

    private var logged: [String] { linesLock.withLock { lines } }

    /// Runs one 10 ms buffer of `frames` samples at 1000.0 through `p`;
    /// returns the first sample afterwards (the fake writes 0 there when it
    /// cleaned).
    @discardableResult
    private func run(
        _ p: LakkoCaptureNoiseProcessor, frames: Int = 480, channels: Int = 1
    ) -> Float {
        var samples = [Float](repeating: 1000, count: frames)
        samples.withUnsafeMutableBufferPointer { buf in
            p.processSamples(buf.baseAddress, frames: frames, channels: channels)
        }
        return samples[0]
    }

    func testCleansAt48kMonoWhenOn() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        XCTAssertTrue(p.running)
        XCTAssertEqual(run(p), 0)
        XCTAssertEqual(engine.processed, [1])
        XCTAssertEqual(p.cleanedCount, 1)
        XCTAssertTrue(logged.contains { $0.contains("RNNoise noise suppression on") })
    }

    func testOffLeavesTheBufferAlone() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: false, platform: "x")
        XCTAssertFalse(p.running)
        XCTAssertEqual(run(p), 1000)
        XCTAssertTrue(engine.created.isEmpty)
        XCTAssertEqual(p.passedCount, 1)
        XCTAssertTrue(logged.contains { $0.contains("noise suppression off, platform processing only") })
    }

    func testAnotherRateIsPassedThroughNotResampled() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 16_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        XCTAssertEqual(run(p, frames: 160), 1000)
        XCTAssertTrue(engine.processed.isEmpty)
        XCTAssertTrue(logged.contains { $0.contains("RNNoise bypassed (needs 48000 Hz mono") })
        XCTAssertTrue(logged.contains { $0.contains("but bypassed: capture runs at 16000 Hz") })
    }

    func testStereoIsPassedThrough() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 2)
        p.arm(forOwner: self, on: true, platform: "x")
        XCTAssertEqual(run(p, channels: 2), 1000)
        XCTAssertTrue(engine.processed.isEmpty)
    }

    func testSwitchAppliesAtOnceWithAFreshSuppressor() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        run(p)
        p.setEnabled(false)
        XCTAssertEqual(engine.freed, [1])
        XCTAssertEqual(run(p), 1000)
        p.setEnabled(true)
        XCTAssertEqual(engine.created, [1, 2])
        XCTAssertEqual(run(p), 0)
        XCTAssertEqual(engine.processed, [1, 2])
        XCTAssertTrue(logged.contains("noise suppression switched off: platform processing only"))
    }

    func testSwitchWithNoCallOnlyAppliesToTheNext() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.setEnabled(true)
        XCTAssertTrue(engine.created.isEmpty)
        XCTAssertFalse(p.running)
    }

    func testARefusedBufferStopsTheSuppressorForTheCallAndLogsOnce() {
        let engine = FakeEngine()
        engine.status = 2
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        XCTAssertEqual(run(p), 1000)
        XCTAssertEqual(run(p), 1000)
        XCTAssertEqual(engine.processed, [1])
        XCTAssertFalse(p.running)
        XCTAssertEqual(logged.filter { $0.contains("noise suppression failed") }.count, 1)
    }

    func testNoNativeSuppressorMeansPlatformOnly() {
        let engine = FakeEngine()
        engine.createFails = true
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        XCTAssertFalse(p.running)
        XCTAssertEqual(run(p), 1000)
        XCTAssertTrue(logged.contains { $0.contains("on but unavailable") })
    }

    func testAStaleDisarmLeavesTheNewCallAlone() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        let oldCall = NSObject()
        let newCall = NSObject()
        p.arm(forOwner: oldCall, on: true, platform: "x")
        p.arm(forOwner: newCall, on: true, platform: "x")
        XCTAssertEqual(engine.freed, [1])
        p.disarm(forOwner: oldCall)
        run(p)
        XCTAssertEqual(engine.processed, [2])
        p.disarm(forOwner: newCall)
        XCTAssertEqual(engine.freed, [1, 2])
        XCTAssertTrue(logged.contains { $0.hasPrefix("call audio ended: 1 buffers cleaned") })
    }

    func testASuppressorIsNeverFreedUnderTheAudioThread() {
        let engine = FakeEngine()
        let p = processor(engine)
        p.audioProcessingInitialize(sampleRate: 48_000, channels: 1)
        p.arm(forOwner: self, on: true, platform: "x")
        let gate = DispatchSemaphore(value: 0)
        let entered = DispatchSemaphore(value: 0)
        engine.gate = gate
        engine.entered = entered
        let audioDone = expectation(description: "audio")
        let controlDone = expectation(description: "control")
        DispatchQueue.global().async {
            self.run(p)
            audioDone.fulfill()
        }
        XCTAssertEqual(entered.wait(timeout: .now() + 5), .success)
        DispatchQueue.global().async {
            p.setEnabled(false)
            controlDone.fulfill()
        }
        usleep(50_000)
        XCTAssertTrue(engine.freed.isEmpty, "freed while in use")
        gate.signal()
        wait(for: [audioDone, controlDone], timeout: 5)
        XCTAssertEqual(engine.processed, [1])
        XCTAssertEqual(engine.freed, [1])
    }

    /// The real RNNoise through the C ABI the app links: a 48 kHz mono 10 ms
    /// buffer is accepted and stays finite.
    func testTheNativeEngineCleansA48kMonoBuffer() {
        let engine = LakkoNativeNoiseEngine()
        XCTAssertEqual(engine.sampleRate, 48_000)
        guard let handle = engine.createSuppressor() else {
            return XCTFail("no native suppressor")
        }
        defer { engine.destroySuppressor(handle) }
        var samples = (0..<480).map { Float(sin(Double($0) * 0.3) * 8000) }
        let status = samples.withUnsafeMutableBufferPointer {
            engine.processSuppressor(handle, samples: $0.baseAddress!, frames: 480)
        }
        XCTAssertEqual(status, LakkoNoiseEngineStatusOK)
        XCTAssertTrue(samples.allSatisfy { $0.isFinite })
    }

    func testTheSwitchIsOnByDefaultStoredAndApplied() throws {
        let suite = "lakko.tests.noise.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        var applied: [Bool] = []
        let setting = NoiseSuppressionSetting(defaults: defaults, apply: { applied.append($0) })
        XCTAssertTrue(setting.isOn)
        XCTAssertTrue(setting.currentChoice)
        setting.set(false)
        XCTAssertFalse(setting.isOn)
        XCTAssertEqual(applied, [false])
        let reread = NoiseSuppressionSetting(defaults: defaults, apply: { _ in })
        XCTAssertFalse(reread.isOn)
        XCTAssertFalse(reread.currentChoice)
    }
}
