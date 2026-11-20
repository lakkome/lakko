// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// ADR-0047 section 6 — the micro thumbnail the first photo or video of a
/// message carries inside the envelope (`AttachmentRef.inlineThumb`), so a
/// notification, the chat list row and the first mosaic tile have a picture
/// without fetching a blob.
///
/// The sender makes it from the same decode that makes the sent `thumb`; a
/// picture that does not fit is left out and the message is sent without it.
public enum InlineThumb {
    /// Longest side in pixels (the ADR's limit; chosen, not measured).
    public static let maxPixel = 96
    /// At most this many bytes (`MAX_INLINE_THUMB_BYTES` in
    /// `lakko-attachments`; the core drops a larger value).
    public static let maxBytes = notificationPictureMaxBytes
    /// The JPEG qualities tried in turn, highest first.
    static let qualities: [Double] = [0.7, 0.55, 0.4, 0.3, 0.2, 0.1]

    /// `bytes` when they are a JPEG (start `FF D8 FF`) of at most
    /// ``maxBytes``; `nil` otherwise. Every reader passes stored or received
    /// bytes through this before a decoder sees them.
    /// The same rule a notification picture passes
    /// (``notificationPicture(_:)``, shared with the notification extension).
    public static func bounded(_ bytes: Data?) -> Data? {
        notificationPicture(bytes)
    }

    /// The first encoding, highest quality first, that fits ``maxBytes``;
    /// `nil` when none does (the message then goes without one).
    static func fit(encode: (Double) -> Data?) -> Data? {
        for quality in qualities {
            if let data = encode(quality), let fitting = bounded(data) { return fitting }
        }
        return nil
    }

    /// The micro thumbnail of `image`: scaled so its longest side is at most
    /// ``maxPixel``, then encoded to fit.
    public static func make(from image: CGImage) -> Data? {
        guard let scaled = downscaled(image) else { return nil }
        return fit { encodeJPEG(scaled, quality: $0) }
    }

    /// The micro thumbnail of an encoded picture (the sender's thumb JPEG).
    public static func make(fromEncoded bytes: Data) -> Data? {
        guard let source = CGImageSourceCreateWithData(bytes as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixel,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return make(from: image)
    }

    /// The micro thumbnail of a video file's first frame.
    public static func make(fromVideo url: URL) async -> Data? {
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: maxPixel, height: maxPixel)
        guard case let (image, _)? = try? await generator.image(at: .zero) else { return nil }
        return make(from: image)
    }

    static func downscaled(_ image: CGImage) -> CGImage? {
        let longest = max(image.width, image.height)
        guard longest > 0 else { return nil }
        guard longest > maxPixel else { return image }
        let scale = Double(maxPixel) / Double(longest)
        let width = max(1, Int((Double(image.width) * scale).rounded()))
        let height = max(1, Int((Double(image.height) * scale).rounded()))
        guard let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return nil }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()
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
}
