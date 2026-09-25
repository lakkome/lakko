// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import os

/// A finished recording, still local.
public struct CapturedTake: Equatable {
    public let kind: CaptureKind
    public let fileURL: URL
    public let durationMs: Int64
    /// Voice only: at most 128 envelope bytes.
    public let waveform: Data?
    /// Video note only: pixel side of the square clip.
    public let side: Int?
    /// Video note only: a JPEG poster frame, at most 256 KiB.
    public let poster: Data?
    public let posterSide: Int?

    /// The attachment MIME type (ADR-0047 §2, one per kind on every platform).
    public var mime: String { kind == .voice ? "audio/mp4" : "video/mp4" }
}

/// ADR-0047 §2 — voice capture: AAC-LC in MP4 (`.m4a`, `audio/mp4`) via
/// `AVAudioRecorder`, with metering sampled while recording for the live level
/// meter and the envelope waveform.
@MainActor
public final class VoiceRecorder: NSObject, AVAudioRecorderDelegate {

    /// Level sampling cadence — the same 50 ms the web recorder uses.
    public static let levelSampleInterval: TimeInterval = 0.05

    private static let log = Logger(subsystem: "me.lakko.media", category: "voice-recorder")

    /// Latest level (0...1) for the live meter, with the recorder's own elapsed
    /// milliseconds at the moment it was taken.
    ///
    /// The time comes from the recorder rather than from the controller's tick
    /// because it is the clock the sample actually belongs to: the tick runs from
    /// the machine's start, which is ``VoiceCapture/inputSettleSeconds`` before
    /// the first captured frame. ``MicrophoneSilence`` measures a run of silence
    /// against it, and a run that began before anything was captured would be
    /// counting the route change as silence the microphone gave.
    public var onLevel: ((Double, Int64) -> Void)?
    /// The recording stopped for a reason other than `stop()` / `discard()`
    /// (an interruption, an encoder error).
    public var onInterrupted: (() -> Void)?

    private var recorder: AVAudioRecorder?
    private var meterTimer: Timer?
    private var levels: [Double] = []
    private var interruptionObserver: NSObjectProtocol?
    private var ownsSession = false

    public var isRecording: Bool { recorder?.isRecording ?? false }

    public func start(directory: URL) throws {
        discard()
        try MediaAudioSession.begin(.record, callActive: false)
        ownsSession = true
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent("voice-\(UUID().uuidString).m4a")
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 44_100,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: 64_000,
            AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
        ]
        let recorder: AVAudioRecorder
        do {
            recorder = try AVAudioRecorder(url: url, settings: settings)
        } catch {
            releaseSession()
            throw error
        }
        recorder.delegate = self
        recorder.isMeteringEnabled = true
        // Create the file and stand the encoder up here, so neither is in the
        // path of the start below.
        guard recorder.prepareToRecord() else {
            releaseSession()
            throw MediaCaptureError.startFailed
        }
        // Schedule the start on the audio device's own clock rather than
        // beginning now: the session was activated a moment ago and the input
        // route is still settling, and anything written during that window is
        // silence the sender never spoke — counted in the duration too, since
        // the take's length is the recorder's clock. See
        // ``VoiceCapture/inputSettleSeconds``.
        guard recorder.record(atTime: recorder.deviceCurrentTime + VoiceCapture.inputSettleSeconds) else {
            releaseSession()
            throw MediaCaptureError.startFailed
        }
        self.recorder = recorder
        levels = []
        meterTimer = Timer.scheduledTimer(withTimeInterval: Self.levelSampleInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.sampleLevel() }
        }
        interruptionObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            let began = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt)
                .flatMap(AVAudioSession.InterruptionType.init(rawValue:)) == .began
            guard began else { return }
            MainActor.assumeIsolated { self?.onInterrupted?() }
        }
    }

    private func sampleLevel() {
        guard let recorder, recorder.isRecording else { return }
        // The timer runs from the moment the recorder was armed, which is
        // before the scheduled start; a tick taken then would prepend bars no
        // sample in the file corresponds to.
        guard VoiceCapture.recordsLevel(recordedSeconds: recorder.currentTime) else { return }
        recorder.updateMeters()
        let level = Waveform.linearLevel(decibels: recorder.averagePower(forChannel: 0))
        levels.append(level)
        onLevel?(level, Int64((recorder.currentTime * 1000).rounded()))
    }

    /// Stop and hand over the take. `nil` when nothing was recording.
    public func stop() -> CapturedTake? {
        guard let recorder else { return nil }
        let duration = Int64((recorder.currentTime * 1000).rounded())
        let url = recorder.url
        tearDown()
        recorder.stop()
        releaseSession()
        return CapturedTake(
            kind: .voice,
            fileURL: url,
            durationMs: duration,
            waveform: Waveform.downsample(levels),
            side: nil,
            poster: nil,
            posterSide: nil
        )
    }

    /// Stop and delete the take.
    public func discard() {
        guard let recorder else { return }
        tearDown()
        recorder.stop()
        recorder.deleteRecording()
        releaseSession()
    }

    private func tearDown() {
        meterTimer?.invalidate()
        meterTimer = nil
        if let interruptionObserver { NotificationCenter.default.removeObserver(interruptionObserver) }
        interruptionObserver = nil
        recorder?.delegate = nil
        recorder = nil
    }

    private func releaseSession() {
        guard ownsSession else { return }
        ownsSession = false
        MediaAudioSession.end(.record)
    }

    nonisolated public func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        Task { @MainActor in
            Self.log.warning("voice recorder encode error")
            self.onInterrupted?()
        }
    }
}

public enum MediaCaptureError: Error, Equatable {
    case startFailed
    case noCamera
    case noMicrophone
    case writerFailed
}
