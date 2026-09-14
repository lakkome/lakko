// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import CoreImage
import Foundation
import os
import UIKit

/// ADR-0047 §2 — video-note capture: H.264 + AAC in MP4 (`video/mp4`), square,
/// at most ``side`` pixels, from the front camera, via `AVCaptureSession` +
/// `AVAssetWriter`.
///
/// The writer's `AVVideoScalingModeResizeAspectFill` does the centred square
/// crop, so every frame the camera delivers — portrait, or after a flip — lands
/// in the same square. The first frame also becomes the poster thumb.
///
/// Threading: every capture callback and every writer call runs on one serial
/// `queue`; the main actor only starts, stops and flips.
public final class VideoNoteRecorder: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate,
    AVCaptureAudioDataOutputSampleBufferDelegate, @unchecked Sendable {

    /// ADR-0047 §2 — a video note is square and at most this many pixels a side.
    public static let side = 480
    /// Poster frame side (the bubble is about 240 pt).
    public static let posterSide = 240

    private static let log = Logger(subsystem: "me.lakko.media", category: "video-note-recorder")

    public static var hasFrontCamera: Bool {
        AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .front) != nil
            || AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) != nil
    }

    public let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "me.lakko.media.video-note")
    private let videoOutput = AVCaptureVideoDataOutput()
    private let audioOutput = AVCaptureAudioDataOutput()
    private let ciContext = CIContext()

    // Queue-confined.
    private var writer: AVAssetWriter?
    private var videoInput: AVAssetWriterInput?
    private var audioInput: AVAssetWriterInput?
    private var firstVideoTime: CMTime?
    private var lastVideoTime: CMTime?
    private var poster: Data?
    private var recording = false
    private var fileURL: URL?
    private var position: AVCaptureDevice.Position = .front

    /// Called on the main actor when capture fails mid-take (an interruption,
    /// a runtime error, a writer failure).
    public var onInterrupted: (@MainActor () -> Void)?

    private var observers: [NSObjectProtocol] = []

    /// Configure the session and start the preview + the take. Blocking work
    /// runs off the main actor.
    public func start(directory: URL) async throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent("video-note-\(UUID().uuidString).mp4")
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            queue.async {
                do {
                    try self.configure(position: .front)
                    self.fileURL = url
                    self.firstVideoTime = nil
                    self.lastVideoTime = nil
                    self.poster = nil
                    self.writer = nil
                    self.recording = true
                    self.session.startRunning()
                    cont.resume()
                } catch {
                    cont.resume(throwing: error)
                }
            }
        }
        await MainActor.run { observeInterruptions() }
    }

    @MainActor
    private func observeInterruptions() {
        let center = NotificationCenter.default
        for name in [AVCaptureSession.runtimeErrorNotification, AVCaptureSession.wasInterruptedNotification] {
            observers.append(center.addObserver(forName: name, object: session, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.onInterrupted?() }
            })
        }
    }

    private func configure(position: AVCaptureDevice.Position) throws {
        session.beginConfiguration()
        defer { session.commitConfiguration() }
        session.sessionPreset = .high
        for input in session.inputs { session.removeInput(input) }
        guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: position)
            ?? AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .unspecified) else {
            throw MediaCaptureError.noCamera
        }
        let cameraInput = try AVCaptureDeviceInput(device: camera)
        guard session.canAddInput(cameraInput) else { throw MediaCaptureError.startFailed }
        session.addInput(cameraInput)
        self.position = camera.position
        guard let mic = AVCaptureDevice.default(for: .audio) else { throw MediaCaptureError.noMicrophone }
        let micInput = try AVCaptureDeviceInput(device: mic)
        guard session.canAddInput(micInput) else { throw MediaCaptureError.startFailed }
        session.addInput(micInput)

        if !session.outputs.contains(videoOutput) {
            videoOutput.alwaysDiscardsLateVideoFrames = true
            videoOutput.setSampleBufferDelegate(self, queue: queue)
            guard session.canAddOutput(videoOutput) else { throw MediaCaptureError.startFailed }
            session.addOutput(videoOutput)
        }
        if !session.outputs.contains(audioOutput) {
            audioOutput.setSampleBufferDelegate(self, queue: queue)
            guard session.canAddOutput(audioOutput) else { throw MediaCaptureError.startFailed }
            session.addOutput(audioOutput)
        }
        if let connection = videoOutput.connection(with: .video) {
            if connection.isVideoRotationAngleSupported(90) { connection.videoRotationAngle = 90 }
        }
    }

    /// Switch front / back camera while locked. The writer keeps writing; the
    /// aspect-fill scaling keeps the square.
    public func flip() {
        queue.async {
            let next: AVCaptureDevice.Position = self.position == .front ? .back : .front
            do {
                try self.configure(position: next)
            } catch {
                Self.log.warning("video note camera flip failed")
            }
        }
    }

    /// Stop and hand over the take, or `nil` when no frame was written.
    public func stop() async -> CapturedTake? {
        let take = await withCheckedContinuation { (cont: CheckedContinuation<CapturedTake?, Never>) in
            queue.async {
                self.recording = false
                self.session.stopRunning()
                guard let writer = self.writer, let url = self.fileURL,
                      let first = self.firstVideoTime, let last = self.lastVideoTime,
                      writer.status == .writing else {
                    self.cancelWriter()
                    cont.resume(returning: nil)
                    return
                }
                self.videoInput?.markAsFinished()
                self.audioInput?.markAsFinished()
                let poster = self.poster
                writer.finishWriting {
                    let ok = writer.status == .completed
                    let duration = Int64((CMTimeGetSeconds(CMTimeSubtract(last, first)) * 1000).rounded())
                    self.queue.async {
                        self.writer = nil
                        self.videoInput = nil
                        self.audioInput = nil
                    }
                    guard ok else {
                        try? FileManager.default.removeItem(at: url)
                        cont.resume(returning: nil)
                        return
                    }
                    cont.resume(returning: CapturedTake(
                        kind: .videoNote,
                        fileURL: url,
                        durationMs: max(0, duration),
                        waveform: nil,
                        side: Self.side,
                        poster: poster,
                        posterSide: poster == nil ? nil : Self.posterSide
                    ))
                }
            }
        }
        await MainActor.run { removeObservers() }
        return take
    }

    /// Stop and delete the take.
    public func discard() {
        queue.async {
            self.recording = false
            self.session.stopRunning()
            self.cancelWriter()
        }
        Task { @MainActor in removeObservers() }
    }

    @MainActor
    private func removeObservers() {
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers.removeAll()
    }

    private func cancelWriter() {
        if let writer, writer.status == .writing { writer.cancelWriting() }
        writer = nil
        videoInput = nil
        audioInput = nil
        if let fileURL { try? FileManager.default.removeItem(at: fileURL) }
    }

    // MARK: - Sample buffers (queue)

    public func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        guard recording, CMSampleBufferDataIsReady(sampleBuffer) else { return }
        let time = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        if output === videoOutput {
            if writer == nil { startWriter(at: time) }
            guard let writer, writer.status == .writing, let videoInput, videoInput.isReadyForMoreMediaData else {
                failIfWriterFailed()
                return
            }
            if videoInput.append(sampleBuffer) {
                lastVideoTime = time
                if poster == nil, let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) {
                    poster = makePoster(pixels)
                }
            } else {
                failIfWriterFailed()
            }
        } else if output === audioOutput {
            // Audio before the first video frame has no place in the file.
            guard let writer, writer.status == .writing, let first = firstVideoTime,
                  CMTimeCompare(time, first) >= 0,
                  let audioInput, audioInput.isReadyForMoreMediaData else {
                return
            }
            _ = audioInput.append(sampleBuffer)
        }
    }

    private func startWriter(at time: CMTime) {
        guard let fileURL else { return }
        do {
            let writer = try AVAssetWriter(outputURL: fileURL, fileType: .mp4)
            writer.shouldOptimizeForNetworkUse = true
            let video = AVAssetWriterInput(mediaType: .video, outputSettings: [
                AVVideoCodecKey: AVVideoCodecType.h264,
                AVVideoWidthKey: Self.side,
                AVVideoHeightKey: Self.side,
                AVVideoScalingModeKey: AVVideoScalingModeResizeAspectFill,
                AVVideoCompressionPropertiesKey: [
                    AVVideoAverageBitRateKey: 1_000_000,
                    AVVideoProfileLevelKey: AVVideoProfileLevelH264MainAutoLevel,
                ],
            ])
            video.expectsMediaDataInRealTime = true
            let audio = AVAssetWriterInput(mediaType: .audio, outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 44_100,
                AVNumberOfChannelsKey: 1,
                AVEncoderBitRateKey: 64_000,
            ])
            audio.expectsMediaDataInRealTime = true
            guard writer.canAdd(video), writer.canAdd(audio) else { throw MediaCaptureError.writerFailed }
            writer.add(video)
            writer.add(audio)
            guard writer.startWriting() else { throw MediaCaptureError.writerFailed }
            writer.startSession(atSourceTime: time)
            self.writer = writer
            self.videoInput = video
            self.audioInput = audio
            self.firstVideoTime = time
        } catch {
            Self.log.warning("video note writer failed to start")
            recording = false
            Task { @MainActor in self.onInterrupted?() }
        }
    }

    private func failIfWriterFailed() {
        guard let writer, writer.status == .failed else { return }
        recording = false
        Task { @MainActor in self.onInterrupted?() }
    }

    /// A centred square JPEG of `posterSide` pixels, at most 256 KiB.
    private func makePoster(_ pixels: CVPixelBuffer) -> Data? {
        var image = CIImage(cvPixelBuffer: pixels)
        let extent = image.extent
        let sideIn = min(extent.width, extent.height)
        guard sideIn >= 2 else { return nil }
        image = image.cropped(to: CGRect(
            x: extent.midX - sideIn / 2,
            y: extent.midY - sideIn / 2,
            width: sideIn,
            height: sideIn
        ))
        // The capture connection already rotates video frames to portrait.
        let scale = CGFloat(Self.posterSide) / sideIn
        image = image.transformed(by: CGAffineTransform(translationX: -image.extent.minX, y: -image.extent.minY))
            .transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        guard let cg = ciContext.createCGImage(image, from: image.extent) else { return nil }
        let ui = UIImage(cgImage: cg)
        for quality in [0.75, 0.6, 0.45, 0.3] {
            if let jpeg = ui.jpegData(compressionQuality: quality), jpeg.count <= AttachmentBridge.maxThumbBytes {
                return jpeg
            }
        }
        return nil
    }
}
