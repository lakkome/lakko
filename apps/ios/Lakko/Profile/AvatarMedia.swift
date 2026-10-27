// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import CoreGraphics
import Foundation
import ImageIO
import UIKit
import UniformTypeIdentifiers

// UX `docs/ux/profile-screen.md` section 2.2 and ADR-0084 section 1: the
// square a person chooses, and the still and video produced from it.

// MARK: - Crop geometry

/// Where a picture sits under the crop circle. Pure.
///
/// The picture is `imageWidth` x `imageHeight` pixels (already upright); the
/// circle is `viewport` points across. At zoom 1 the picture's short side
/// fills the circle; zoom goes up to 4. `offset` moves the picture's centre
/// from the circle's centre, in points, and is clamped so the circle never
/// leaves the picture.
struct AvatarCropGeometry: Equatable {
    static let maxZoom: CGFloat = 4

    let imageWidth: CGFloat
    let imageHeight: CGFloat
    let viewport: CGFloat
    private(set) var zoom: CGFloat = 1
    private(set) var offset: CGSize = .zero

    init(imageWidth: CGFloat, imageHeight: CGFloat, viewport: CGFloat) {
        self.imageWidth = max(1, imageWidth)
        self.imageHeight = max(1, imageHeight)
        self.viewport = max(1, viewport)
    }

    /// Points per pixel at zoom 1.
    var baseScale: CGFloat { viewport / min(imageWidth, imageHeight) }
    /// Points per pixel now.
    var scale: CGFloat { baseScale * zoom }
    /// The picture's size on screen.
    var displayedSize: CGSize { CGSize(width: imageWidth * scale, height: imageHeight * scale) }

    mutating func setZoom(_ value: CGFloat) {
        zoom = min(Self.maxZoom, max(1, value))
        offset = clamp(offset)
    }

    mutating func setOffset(_ value: CGSize) {
        offset = clamp(value)
    }

    func clamp(_ value: CGSize) -> CGSize {
        let maxX = max(0, (imageWidth * scale - viewport) / 2)
        let maxY = max(0, (imageHeight * scale - viewport) / 2)
        return CGSize(
            width: min(maxX, max(-maxX, value.width)),
            height: min(maxY, max(-maxY, value.height)))
    }

    /// The square of the picture under the circle, in pixels.
    var cropRect: CGRect {
        let side = min(viewport / scale, min(imageWidth, imageHeight))
        let centreX = imageWidth / 2 - offset.width / scale
        let centreY = imageHeight / 2 - offset.height / scale
        let x = min(max(0, centreX - side / 2), imageWidth - side)
        let y = min(max(0, centreY - side / 2), imageHeight - side)
        return CGRect(x: x, y: y, width: side, height: side)
    }

    /// The side the output is drawn at: the crop's own pixels, never scaled
    /// up, at most `maxSide`, at least 1.
    static func outputSide(cropSide: CGFloat, maxSide: Int) -> Int {
        max(1, min(maxSide, Int(cropSide.rounded(.down))))
    }
}

// MARK: - The still

enum AvatarJPEG {
    /// The qualities tried in order until the bytes fit.
    static let qualities: [Double] = [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3]
    /// Below this side a smaller picture is not tried.
    static let minimumSide = 64

    /// The first encoding at a decreasing quality that fits `maxBytes`, or
    /// `nil` when none does.
    static func fit(maxBytes: Int, encode: (Double) -> Data?) -> Data? {
        for quality in qualities {
            if let data = encode(quality), !data.isEmpty, data.count <= maxBytes { return data }
        }
        return nil
    }

    /// A square JPEG of `image` (already square) at most `maxSide` and
    /// `maxBytes`: quality first, then a smaller side.
    static func encodeSquare(_ image: CGImage, maxSide: Int, maxBytes: Int) -> (data: Data, side: Int)? {
        var side = min(maxSide, image.width)
        while side >= minimumSide || side == image.width {
            guard let scaled = draw(image, crop: CGRect(x: 0, y: 0, width: image.width, height: image.height),
                                    side: side) else { return nil }
            if let data = fit(maxBytes: maxBytes, encode: { jpeg(scaled, quality: $0) }) { return (data, side) }
            if side <= minimumSide { break }
            side = max(minimumSide, Int(Double(side) * 0.8))
        }
        return nil
    }

    /// `crop` of `image` drawn into a `side` x `side` opaque bitmap.
    static func draw(_ image: CGImage, crop: CGRect, side: Int) -> CGImage? {
        guard side >= 1, let cropped = image.cropping(to: crop.integral) else { return nil }
        guard let context = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        context.interpolationQuality = .high
        context.draw(cropped, in: CGRect(x: 0, y: 0, width: side, height: side))
        return context.makeImage()
    }

    static func jpeg(_ image: CGImage, quality: Double) -> Data? {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    /// The upright bitmap of a picked or captured picture.
    static func upright(_ image: UIImage) -> CGImage? {
        if image.imageOrientation == .up, let cg = image.cgImage { return cg }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let size = CGSize(width: image.size.width * image.scale, height: image.size.height * image.scale)
        return UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }.cgImage
    }

    /// The photo path of `Use`: the crop, at most 640 px, at most 256 KiB.
    static func still(from image: CGImage, crop: CGRect) -> (data: Data, side: Int)? {
        let side = AvatarCropGeometry.outputSide(cropSide: crop.width, maxSide: AvatarLimits.stillMaxSide)
        guard let square = draw(image, crop: crop, side: side) else { return nil }
        return encodeSquare(square, maxSide: side, maxBytes: AvatarLimits.stillMaxBytes)
    }
}

// MARK: - The video

/// A picked video, loaded: its upright size and length.
struct AvatarVideoSource {
    let asset: AVURLAsset
    let track: AVAssetTrack
    /// Upright pixel size.
    let size: CGSize
    let preferredTransform: CGAffineTransform
    let naturalSize: CGSize
    let durationSeconds: Double

    /// `nil` when the file is not a video this device can play.
    static func load(_ url: URL) async -> AvatarVideoSource? {
        let asset = AVURLAsset(url: url)
        guard (try? await asset.load(.isPlayable)) == true,
              let track = try? await asset.loadTracks(withMediaType: .video).first,
              case let (natural, transform)? = try? await track.load(.naturalSize, .preferredTransform),
              let duration = try? await asset.load(.duration) else { return nil }
        let rect = CGRect(origin: .zero, size: natural).applying(transform)
        let size = CGSize(width: abs(rect.width).rounded(), height: abs(rect.height).rounded())
        let seconds = CMTimeGetSeconds(duration)
        guard size.width >= 1, size.height >= 1, seconds.isFinite, seconds > 0 else { return nil }
        return AvatarVideoSource(
            asset: asset, track: track, size: size, preferredTransform: transform, naturalSize: natural,
            durationSeconds: seconds)
    }

    /// The upright frame at `seconds`.
    func frame(at seconds: Double, maximumSize: CGSize = .zero) async -> CGImage? {
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = CMTime(value: 1, timescale: 10)
        generator.requestedTimeToleranceAfter = CMTime(value: 1, timescale: 10)
        if maximumSize != .zero { generator.maximumSize = maximumSize }
        let time = CMTime(seconds: max(0, seconds), preferredTimescale: 600)
        return try? await generator.image(at: time).image
    }
}

/// The 10 s window a video keeps. Pure.
enum AvatarVideoWindow {
    static let maxSeconds = Double(AvatarLimits.videoMaxDurationMs) / 1000

    /// The kept length: the whole video when it is short enough.
    static func length(durationSeconds: Double) -> Double {
        min(maxSeconds, max(0, durationSeconds))
    }

    /// A start the window fits after.
    static func clampStart(_ start: Double, durationSeconds: Double) -> Double {
        min(max(0, durationSeconds - length(durationSeconds: durationSeconds)), max(0, start))
    }

    /// The kept length in whole milliseconds, 1...10000.
    static func durationMs(durationSeconds: Double) -> Int {
        max(1, min(AvatarLimits.videoMaxDurationMs, Int((length(durationSeconds: durationSeconds) * 1000).rounded(.down))))
    }
}

enum AvatarVideoExportError: Error { case unusable }

enum AvatarVideoExport {
    /// The bitrates tried until the file fits 4 MiB.
    static let bitrates = [1_200_000, 800_000, 500_000]

    /// A square H.264 MP4 without sound of `crop` (upright pixels) from
    /// `start` for at most 10 s, at most 480 px, at most 4 MiB, and the JPEG
    /// still of its first frame.
    static func export(
        _ source: AvatarVideoSource, crop: CGRect, start: Double, directory: URL
    ) async throws -> PendingAvatarMedia {
        let length = AvatarVideoWindow.length(durationSeconds: source.durationSeconds - start)
        guard length > 0 else { throw AvatarVideoExportError.unusable }
        var side = AvatarCropGeometry.outputSide(cropSide: crop.width, maxSide: AvatarLimits.videoMaxSide)
        side -= side % 2
        guard side >= 2 else { throw AvatarVideoExportError.unusable }

        // Only the video track goes into the composition: no sound is kept.
        let composition = AVMutableComposition()
        guard let track = composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid)
        else { throw AvatarVideoExportError.unusable }
        let range = CMTimeRange(
            start: CMTime(seconds: start, preferredTimescale: 600),
            duration: CMTime(seconds: length, preferredTimescale: 600))
        try track.insertTimeRange(range, of: source.track, at: .zero)

        let scale = CGFloat(side) / crop.width
        let instruction = AVMutableVideoCompositionInstruction()
        instruction.timeRange = CMTimeRange(start: .zero, duration: range.duration)
        let layer = AVMutableVideoCompositionLayerInstruction(assetTrack: track)
        // natural -> upright, then the crop's corner to the origin, then the
        // output size.
        let uprightOrigin = CGRect(origin: .zero, size: source.naturalSize)
            .applying(source.preferredTransform).origin
        let transform = source.preferredTransform
            .concatenating(CGAffineTransform(translationX: -uprightOrigin.x - crop.minX, y: -uprightOrigin.y - crop.minY))
            .concatenating(CGAffineTransform(scaleX: scale, y: scale))
        layer.setTransform(transform, at: .zero)
        instruction.layerInstructions = [layer]
        let videoComposition = AVMutableVideoComposition()
        videoComposition.renderSize = CGSize(width: side, height: side)
        videoComposition.frameDuration = CMTime(value: 1, timescale: 30)
        videoComposition.instructions = [instruction]

        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for bitrate in bitrates {
            let url = directory.appendingPathComponent("avatar-\(UUID().uuidString).mp4")
            do {
                try await write(composition: composition, video: videoComposition, side: side, bitrate: bitrate, to: url)
            } catch {
                try? FileManager.default.removeItem(at: url)
                throw AvatarVideoExportError.unusable
            }
            let bytes = ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.intValue ?? 0
            guard bytes >= 1, bytes <= AvatarLimits.videoMaxBytes else {
                try? FileManager.default.removeItem(at: url)
                continue
            }
            guard let output = await AvatarVideoSource.load(url),
                  let first = await output.frame(at: 0),
                  let still = AvatarJPEG.encodeSquare(
                      first, maxSide: min(AvatarLimits.stillMaxSide, first.width),
                      maxBytes: AvatarLimits.stillMaxBytes) else {
                try? FileManager.default.removeItem(at: url)
                throw AvatarVideoExportError.unusable
            }
            return PendingAvatarMedia(
                stillJPEG: still.data, stillSide: still.side,
                videoFile: url, videoSide: side,
                videoDurationMs: AvatarVideoWindow.durationMs(durationSeconds: output.durationSeconds))
        }
        throw AvatarVideoExportError.unusable
    }

    private static func write(
        composition: AVComposition, video: AVVideoComposition, side: Int, bitrate: Int, to url: URL
    ) async throws {
        let reader = try AVAssetReader(asset: composition)
        let tracks = composition.tracks(withMediaType: .video)
        let output = AVAssetReaderVideoCompositionOutput(
            videoTracks: tracks,
            videoSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA])
        output.videoComposition = video
        guard reader.canAdd(output) else { throw AvatarVideoExportError.unusable }
        reader.add(output)
        let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        writer.shouldOptimizeForNetworkUse = true
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: side,
            AVVideoHeightKey: side,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: bitrate,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264MainAutoLevel,
            ],
        ])
        input.expectsMediaDataInRealTime = false
        guard writer.canAdd(input) else { throw AvatarVideoExportError.unusable }
        writer.add(input)
        guard reader.startReading(), writer.startWriting() else { throw AvatarVideoExportError.unusable }
        writer.startSession(atSourceTime: .zero)
        let queue = DispatchQueue(label: "me.lakko.profile.avatar-export")
        let box = UncheckedSendableBox((reader, writer, input, output))
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            box.value.2.requestMediaDataWhenReady(on: queue) {
                let (reader, _, input, output) = box.value
                while input.isReadyForMoreMediaData {
                    if let sample = output.copyNextSampleBuffer() {
                        if !input.append(sample) {
                            reader.cancelReading()
                            input.markAsFinished()
                            cont.resume()
                            return
                        }
                    } else {
                        input.markAsFinished()
                        cont.resume()
                        return
                    }
                }
            }
        }
        await writer.finishWriting()
        guard reader.status == .completed, writer.status == .completed else { throw AvatarVideoExportError.unusable }
    }
}

/// Carries non-Sendable AVFoundation objects into the writer's queue, where
/// only that queue touches them.
private final class UncheckedSendableBox<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}

/// Where prepared profile videos wait for Save.
enum AvatarMediaFiles {
    static let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent("profile-avatar", isDirectory: true)
}
