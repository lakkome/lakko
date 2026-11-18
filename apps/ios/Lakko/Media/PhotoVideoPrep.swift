// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import CoreTransferable
import Foundation
import LakkoCore
import ImageIO
import os
import UIKit
import UniformTypeIdentifiers

/// UX spec section 10 — what the sender prepares for a picked photo or video
/// before it is encrypted: a format every client renders inline (HEIC becomes
/// JPEG, a MOV becomes MP4), the pixel size, and a small JPEG preview.
///
/// A preview that cannot be produced is simply absent: a send is never
/// blocked on it.
public enum PhotoVideoPrep {

    private static let log = Logger(subsystem: "me.lakko.media", category: "prep")

    /// Longest side of a thumb or poster, in pixels.
    public static let thumbMaxPixel = 320
    /// Picked and exported videos before they are sent; under the media temp
    /// parent, so the launch sweep removes anything a crash left.
    public static let pickedVideoDirectory = MediaTempFiles.defaultParent
        .appendingPathComponent("picked-video", isDirectory: true)
    /// The size a thumb aims for.
    public static let thumbTargetBytes = 64 * 1024

    public struct Prepared: Sendable {
        public var mime: String
        public var name: String
        /// In memory for a photo; `nil` for a video, which is ``fileURL``.
        public var bytes: Data?
        public var fileURL: URL?
        public var width: UInt32?
        public var height: UInt32?
        public var thumb: Data?
        public var thumbWidth: UInt32?
        public var thumbHeight: UInt32?

        public var isVideo: Bool { fileURL != nil }
    }

    // MARK: - Photo

    /// Magic-byte sniff for the four inline image formats.
    public static func sniffInlineImageMime(_ bytes: Data) -> String? {
        guard bytes.count >= 12 else { return nil }
        let b = [UInt8](bytes.prefix(12))
        if b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF { return "image/jpeg" }
        if b[0] == 0x89 && b[1] == 0x50 && b[2] == 0x4E && b[3] == 0x47 { return "image/png" }
        if b[0] == 0x47 && b[1] == 0x49 && b[2] == 0x46 && b[3] == 0x38
            && (b[4] == 0x37 || b[4] == 0x39) && b[5] == 0x61 {
            return "image/gif"
        }
        if b[0] == 0x52 && b[1] == 0x49 && b[2] == 0x46 && b[3] == 0x46
            && b[8] == 0x57 && b[9] == 0x45 && b[10] == 0x42 && b[11] == 0x50 {
            return "image/webp"
        }
        return nil
    }

    /// A picked image, ready to send as a photo bubble. HEIC / HEIF (and any
    /// other format ImageIO decodes but the inline allowlist refuses) is
    /// re-encoded as JPEG with the orientation applied. `nil` when the bytes
    /// are not a decodable image; the caller then sends them as a file.
    public static func preparePhoto(_ bytes: Data, stamp: String) -> Prepared? {
        guard let source = CGImageSourceCreateWithData(bytes as CFData, nil),
              CGImageSourceGetCount(source) > 0 else { return nil }
        var payload = bytes
        var mime = sniffInlineImageMime(bytes)
        // ADR-0090 section 9 / UX section 3 — every photo but an animated GIF
        // is re-encoded as JPEG with its longest side at most the core's
        // target (2560 px), which drops its metadata (EXIF location, camera,
        // time): the re-encode writes the pixels only.
        if mime != "image/gif" {
            guard let jpeg = transcodeToJPEG(source, maxSide: Int(photoMaxLongestSidePx())) else { return nil }
            payload = jpeg
            mime = "image/jpeg"
        }
        guard let mime else { return nil }
        let ext: String
        switch mime {
        case "image/png": ext = "png"
        case "image/gif": ext = "gif"
        case "image/webp": ext = "webp"
        default: ext = "jpg"
        }
        var prepared = Prepared(mime: mime, name: "photo-\(stamp).\(ext)", bytes: payload)
        let finalSource = payload == bytes ? source : (CGImageSourceCreateWithData(payload as CFData, nil) ?? source)
        if let size = orientedPixelSize(finalSource),
           let wire = InlineMedia.wireDimensions(width: size.width, height: size.height) {
            prepared.width = wire.width
            prepared.height = wire.height
        }
        if let thumb = thumbnailJPEG(finalSource) {
            prepared.thumb = thumb.data
            prepared.thumbWidth = thumb.width
            prepared.thumbHeight = thumb.height
        }
        return prepared
    }

    static func orientedPixelSize(_ source: CGImageSource) -> (width: Int, height: Int)? {
        guard let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let w = (props[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue,
              let h = (props[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue else { return nil }
        let orientation = (props[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
        // EXIF orientations 5-8 rotate by 90 degrees.
        return (5...8).contains(orientation) ? (h, w) : (w, h)
    }

    static func transcodeToJPEG(_ source: CGImageSource, maxSide: Int = .max) -> Data? {
        guard let size = orientedPixelSize(source) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: min(max(size.width, size.height), maxSide),
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return encodeJPEG(cg, quality: 0.9)
    }

    private static func thumbnailJPEG(_ source: CGImageSource) -> (data: Data, width: UInt32, height: UInt32)? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: thumbMaxPixel,
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return fitThumb(cg)
    }

    /// Encode `image` as JPEG at a quality that lands under
    /// ``thumbTargetBytes``, accepting anything under the wire cap.
    static func fitThumb(_ image: CGImage) -> (data: Data, width: UInt32, height: UInt32)? {
        var best: Data?
        for quality in [0.75, 0.6, 0.45, 0.3, 0.2] {
            guard let data = encodeJPEG(image, quality: quality) else { continue }
            best = data
            if data.count <= thumbTargetBytes { break }
        }
        guard let best, best.count <= AttachmentBridge.maxThumbBytes else { return nil }
        return (best, UInt32(image.width), UInt32(image.height))
    }

    private static func encodeJPEG(_ image: CGImage, quality: Double) -> Data? {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    // MARK: - Video

    /// A picked video file, ready to send as a video bubble: an MP4 (exported
    /// from another container when needed, at 720p if the original would not
    /// fit the attachment ceiling), its pixel size and a first-frame poster.
    /// `nil` when no MP4 under the ceiling can be produced.
    public static func prepareVideo(_ source: URL, stamp: String, allowReencode: Bool = true) async -> Prepared? {
        let asset = AVURLAsset(url: source)
        guard let mp4 = await exportMP4(asset: asset, source: source, allowReencode: allowReencode) else { return nil }
        var prepared = Prepared(mime: "video/mp4", name: "video-\(stamp).mp4", fileURL: mp4)
        let output = AVURLAsset(url: mp4)
        if let track = try? await output.loadTracks(withMediaType: .video).first,
           case let (natural, transform)? = try? await track.load(.naturalSize, .preferredTransform) {
            let rect = CGRect(origin: .zero, size: natural).applying(transform)
            if let wire = InlineMedia.wireDimensions(
                width: Int(abs(rect.width).rounded()),
                height: Int(abs(rect.height).rounded())
            ) {
                prepared.width = wire.width
                prepared.height = wire.height
            }
        }
        let generator = AVAssetImageGenerator(asset: output)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: thumbMaxPixel, height: thumbMaxPixel)
        if case let (image, _)? = try? await generator.image(at: .zero), let thumb = fitThumb(image) {
            prepared.thumb = thumb.data
            prepared.thumbWidth = thumb.width
            prepared.thumbHeight = thumb.height
        } else {
            log.warning("video poster unavailable; sending without it")
        }
        return prepared
    }

    private static func fileSize(_ url: URL) -> Int {
        ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.intValue ?? 0
    }

    private static func exportMP4(asset: AVURLAsset, source: URL, allowReencode: Bool = true) async -> URL? {
        let dir = PhotoVideoPrep.pickedVideoDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let isMP4 = UTType(filenameExtension: source.pathExtension)?.conforms(to: .mpeg4Movie) == true
        if isMP4, fileSize(source) <= AttachmentBridge.maxAttachmentBytes {
            let copy = dir.appendingPathComponent("video.mp4")
            if (try? FileManager.default.copyItem(at: source, to: copy)) != nil { return copy }
        }
        // ADR-0090 / attachments UX section 3 — the send preview sends a
        // video as picked (no re-encode in v1): only the copy above.
        // Without re-encoding only the passthrough remux (an H.264 QuickTime
        // file into an MP4 container) is tried.
        let presets = allowReencode
            ? [AVAssetExportPresetPassthrough, AVAssetExportPreset1280x720, AVAssetExportPreset960x540]
            : [AVAssetExportPresetPassthrough]
        for preset in presets {
            let out = dir.appendingPathComponent("video-\(UUID().uuidString).mp4")
            guard let session = AVAssetExportSession(asset: asset, presetName: preset) else { continue }
            session.outputURL = out
            session.outputFileType = .mp4
            session.shouldOptimizeForNetworkUse = true
            await session.export()
            guard session.status == .completed else {
                try? FileManager.default.removeItem(at: out)
                continue
            }
            if fileSize(out) <= AttachmentBridge.maxAttachmentBytes { return out }
            try? FileManager.default.removeItem(at: out)
        }
        log.warning("video export produced no mp4 under the attachment ceiling")
        return nil
    }
}

/// A video picked from the photo library, copied to a temp file the app owns.
struct PickedMovie: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(contentType: .movie) { movie in
            SentTransferredFile(movie.url)
        } importing: { received in
            let dir = PhotoVideoPrep.pickedVideoDirectory
                .appendingPathComponent(UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let ext = received.file.pathExtension.isEmpty ? "mov" : received.file.pathExtension
            let target = dir.appendingPathComponent("picked.\(ext)")
            try FileManager.default.copyItem(at: received.file, to: target)
            return PickedMovie(url: target)
        }
    }
}
