// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import ImageIO
import LakkoCore
import os
import UIKit

/// UX spec section 10 — decrypted photo bytes, thumbs and outgoing video
/// files, kept for the app session so scrolling back or reopening a chat does
/// not download again.
///
/// Photos and thumbs live in memory (an `NSCache`, evicted under pressure);
/// a video this device sent is a file under ``MediaTempFiles/defaultParent``,
/// which the launch sweep removes. Only two kinds of bytes ever enter:
/// the output of `AttachmentBridge.downloadAndDecrypt` (AEAD-authenticated
/// before a byte returns) and bytes this device itself just sent.
@MainActor
public final class MediaSessionCache {

    public static let shared = MediaSessionCache()

    private static let log = Logger(subsystem: "me.lakko.media", category: "session-cache")

    private let data = NSCache<NSString, NSData>()
    private let images = NSCache<NSString, UIImage>()
    private var inflight: [String: Task<Data, Error>] = [:]
    private var videoFiles: [String: URL] = [:]
    /// Bumped by ``purge()``, so a download that started before it does not
    /// put its bytes back into the cache when it lands.
    private var generation = 0

    nonisolated static let videoDirectory = MediaTempFiles.defaultParent
        .appendingPathComponent("session-video", isDirectory: true)

    init() {
        data.totalCostLimit = 256 * 1024 * 1024
        images.totalCostLimit = 128 * 1024 * 1024
    }

    // MARK: - Keys

    public static func key(for ref: AttachmentRef) -> String { ChatMediaController.key(for: ref) }

    public static func key(for thumb: ThumbRef) -> String { ChatMediaController.thumbKey(for: thumb) }

    // MARK: - Bytes

    public func cachedData(_ key: String) -> Data? {
        data.object(forKey: key as NSString) as Data?
    }

    public func store(_ bytes: Data, key: String) {
        data.setObject(bytes as NSData, forKey: key as NSString, cost: bytes.count)
    }

    /// The decrypted thumb, or `nil` when it is not an allowlisted image or
    /// fails to download / authenticate.
    public func thumbData(_ thumb: ThumbRef) async -> Data? {
        guard InlineMedia.isImageThumb(thumb) else { return nil }
        let key = Self.key(for: thumb)
        if let hit = cachedData(key) { return hit }
        return try? await single(key) {
            try await AttachmentBridge.downloadAndDecrypt(thumb: thumb)
        }
    }

    /// The decrypted full photo. Refuses anything that is not a photo bubble,
    /// so the auto-load can never fetch a file the user did not ask for.
    public func photoData(_ ref: AttachmentRef) async throws -> Data {
        guard InlineMedia.decision(for: ref).bubble == .photo else {
            throw AttachmentBridgeError.missing(blobId: ref.blobId)
        }
        let key = Self.key(for: ref)
        if let hit = cachedData(key) { return hit }
        return try await single(key) {
            try await AttachmentBridge.downloadAndDecrypt(ref: ref)
        }
    }

    private func single(_ key: String, _ work: @escaping @Sendable () async throws -> Data) async throws -> Data {
        if let task = inflight[key] { return try await task.value }
        let task = Task { try await work() }
        inflight[key] = task
        let startedIn = generation
        defer { if startedIn == generation { inflight.removeValue(forKey: key) } }
        do {
            let bytes = try await task.value
            if startedIn == generation { store(bytes, key: key) }
            return bytes
        } catch {
            Self.log.warning("media download failed")
            throw error
        }
    }

    // MARK: - Display images

    /// A decoded image no larger than `maxPixel` on its longest side, cached
    /// per key and size. Decoding runs off the main actor.
    public func displayImage(key: String, bytes: Data, maxPixel: Int) async -> UIImage? {
        let cacheKey = "\(key)@\(maxPixel)" as NSString
        if let hit = images.object(forKey: cacheKey) { return hit }
        let startedIn = generation
        let image = await Task.detached(priority: .userInitiated) {
            Self.downsample(bytes, maxPixel: maxPixel)
        }.value
        if startedIn == generation, let image, let cg = image.cgImage {
            images.setObject(image, forKey: cacheKey, cost: cg.bytesPerRow * cg.height)
        }
        return image
    }

    /// ImageIO downsample with the EXIF orientation applied. `nil` when the
    /// bytes do not decode.
    nonisolated public static func downsample(_ bytes: Data, maxPixel: Int) -> UIImage? {
        let options = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(bytes as CFData, options) else { return nil }
        let thumbOptions: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: max(1, maxPixel),
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbOptions as CFDictionary) else {
            return nil
        }
        return UIImage(cgImage: cg)
    }

    /// Forget device / Delete account (``AccountLocalDataWipe``): drop every
    /// decrypted byte and image held for the session, and forget the video
    /// files (the wipe removes their folder with the rest of the media temp
    /// directory). A download or decode still in flight finishes into its caller only.
    public func purge() {
        generation += 1
        data.removeAllObjects()
        images.removeAllObjects()
        for task in inflight.values { task.cancel() }
        inflight.removeAll()
        videoFiles.removeAll()
    }

    // MARK: - Video files

    /// A video file this device sent, filed under its blob id.
    public func videoFile(_ key: String) -> URL? {
        guard let url = videoFiles[key] else { return nil }
        if FileManager.default.fileExists(atPath: url.path) { return url }
        videoFiles.removeValue(forKey: key)
        return nil
    }

    public func adoptVideoFile(_ source: URL, key: String) {
        let dir = Self.videoDirectory.appendingPathComponent(MediaTempFiles.directoryName(for: key), isDirectory: true)
        let target = dir.appendingPathComponent("video.mp4", isDirectory: false)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if FileManager.default.fileExists(atPath: target.path) {
                try FileManager.default.removeItem(at: target)
            }
            try FileManager.default.moveItem(at: source, to: target)
            videoFiles[key] = target
        } catch {
            try? FileManager.default.removeItem(at: source)
        }
    }
}
