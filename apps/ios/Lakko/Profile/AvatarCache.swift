// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Combine
import Foundation
import ImageIO
import LakkoCore
import UIKit
import UniformTypeIdentifiers

/// ADR-0084 section 3 — decrypted avatars on this device, keyed by blob id.
///
/// One file per blob under Application Support (`avatars/`), written with the
/// same Data Protection class the decrypted chat media uses
/// (`.completeFileProtection`: encrypted at rest, unreadable while the device
/// is locked) and excluded from backups. Only bytes that passed the AEAD open
/// AND the format check are ever written, so a file here is a picture that
/// decodes. A blob held is never fetched again.
struct AvatarDiskCache: @unchecked Sendable {
    enum Kind: String { case still = "jpg", video = "mp4" }

    let directory: URL
    private let fileManager: FileManager

    static let defaultDirectory: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("avatars", isDirectory: true)
    }()

    init(directory: URL = AvatarDiskCache.defaultDirectory, fileManager: FileManager = .default) {
        self.directory = directory
        self.fileManager = fileManager
    }

    /// The file for `blobIdHex`, whether or not it exists. The name is the
    /// hex of a 32-byte id, never anything a sender chose.
    func url(blobIdHex: String, kind: Kind) -> URL {
        directory.appendingPathComponent("\(blobIdHex).\(kind.rawValue)", isDirectory: false)
    }

    /// The file when it is held.
    func existing(blobIdHex: String, kind: Kind) -> URL? {
        let url = url(blobIdHex: blobIdHex, kind: kind)
        return fileManager.fileExists(atPath: url.path) ? url : nil
    }

    func read(blobIdHex: String, kind: Kind) -> Data? {
        existing(blobIdHex: blobIdHex, kind: kind).flatMap { try? Data(contentsOf: $0) }
    }

    @discardableResult
    func write(_ data: Data, blobIdHex: String, kind: Kind) throws -> URL {
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        var dir = directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? dir.setResourceValues(values)
        let url = url(blobIdHex: blobIdHex, kind: kind)
        try data.write(to: url, options: [.atomic, .completeFileProtection])
        return url
    }

    /// Forget device: every avatar goes.
    func purge() {
        try? fileManager.removeItem(at: directory)
    }
}

/// When a failed avatar may be fetched again (UX spec section 7: "tried
/// again on the next display after 1 minute").
enum AvatarRetryPolicy {
    static let retryAfter: TimeInterval = 60

    static func mayFetch(lastFailure: Date?, now: Date) -> Bool {
        guard let lastFailure else { return true }
        return now.timeIntervalSince(lastFailure) >= retryAfter
    }
}

/// The checks a decrypted avatar must pass before it is shown or kept.
enum AvatarFormatCheck {
    /// A still is a JPEG ImageIO decodes, square, at the size the profile
    /// stated.
    static func still(_ data: Data, ref: AvatarBlobRef) -> UIImage? {
        guard data.count == Int(ref.size),
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let type = CGImageSourceGetType(source) as String?,
              type == UTType.jpeg.identifier,
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
              image.width == image.height, image.width >= 1 else { return nil }
        return UIImage(cgImage: image)
    }

    /// A video is an MP4 with a playable video track.
    static func video(at url: URL, ref: AvatarBlobRef) async -> Bool {
        guard let size = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber,
              size.intValue == Int(ref.size) else { return false }
        let asset = AVURLAsset(url: url)
        guard let playable = try? await asset.load(.isPlayable), playable,
              let tracks = try? await asset.loadTracks(withMediaType: .video), !tracks.isEmpty
        else { return false }
        return true
    }
}

/// ADR-0084 — the app's avatars: what each person's profile names, fetched
/// lazily on first display, verified, cached on disk, held in memory.
///
/// App target only (the NotificationServiceExtension compiles none of
/// this), and loads are started only from a view that is drawn, so nothing
/// is ever fetched from a background path. A failure keeps the initials and
/// is retried on a display at least a minute later.
@MainActor
final class AvatarImages: ObservableObject {
    static let shared = AvatarImages()

    /// Bumped when a picture arrives; every avatar view re-renders.
    @Published private(set) var revision: UInt64 = 0

    private let disk: AvatarDiskCache
    private let images = NSCache<NSString, UIImage>()
    private var failures: [String: Date] = [:]
    private var inflight: Set<String> = []

    /// Download and decrypt one blob. A seam for tests.
    var fetch: @Sendable (AttachmentRef) async throws -> Data = {
        try await AttachmentBridge.downloadAndDecrypt(ref: $0)
    }
    var now: () -> Date = Date.init

    init(disk: AvatarDiskCache = AvatarDiskCache()) {
        self.disk = disk
        images.countLimit = 300
    }

    /// The still of `avatar`, or `nil` (then a load starts unless one runs,
    /// or failed less than a minute ago).
    func still(_ avatar: ProfileAvatar) -> UIImage? {
        let ref = avatar.still
        let hex = ref.blobIdHex
        if let image = images.object(forKey: hex as NSString) { return image }
        if let data = disk.read(blobIdHex: hex, kind: .still), let image = AvatarFormatCheck.still(data, ref: ref) {
            images.setObject(image, forKey: hex as NSString)
            return image
        }
        load(ref, kind: .still)
        return nil
    }

    /// The local file of `avatar`'s video, or `nil` (a load starts as for the
    /// still).
    func videoURL(_ avatar: ProfileAvatar) -> URL? {
        guard let ref = avatar.video else { return nil }
        if let url = disk.existing(blobIdHex: ref.blobIdHex, kind: .video) { return url }
        load(ref, kind: .video)
        return nil
    }

    /// Bytes this device itself just uploaded: kept at once, so the own
    /// avatar shows without a download.
    func adopt(stillJPEG: Data, videoFile: URL?, avatar: ProfileAvatar) {
        if let url = try? disk.write(stillJPEG, blobIdHex: avatar.still.blobIdHex, kind: .still),
           let data = try? Data(contentsOf: url), let image = UIImage(data: data) {
            images.setObject(image, forKey: avatar.still.blobIdHex as NSString)
        }
        if let videoFile, let ref = avatar.video, let data = try? Data(contentsOf: videoFile) {
            _ = try? disk.write(data, blobIdHex: ref.blobIdHex, kind: .video)
        }
        revision &+= 1
    }

    /// Forget device.
    func purge() {
        images.removeAllObjects()
        failures.removeAll()
        disk.purge()
        revision &+= 1
    }

    private func load(_ ref: AvatarBlobRef, kind: AvatarDiskCache.Kind) {
        let key = "\(ref.blobIdHex).\(kind.rawValue)"
        guard !inflight.contains(key),
              AvatarRetryPolicy.mayFetch(lastFailure: failures[key], now: now()) else { return }
        inflight.insert(key)
        let fetch = self.fetch
        let disk = self.disk
        Task { @MainActor in
            defer { inflight.remove(key) }
            do {
                let data = try await fetch(ref.attachmentRef)
                switch kind {
                case .still:
                    guard let image = AvatarFormatCheck.still(data, ref: ref) else { throw AvatarLoadRefused() }
                    try disk.write(data, blobIdHex: ref.blobIdHex, kind: .still)
                    images.setObject(image, forKey: ref.blobIdHex as NSString)
                case .video:
                    // Checked from a scratch file; kept only once it passes.
                    let scratch = FileManager.default.temporaryDirectory
                        .appendingPathComponent("avatar-check-\(UUID().uuidString).mp4")
                    try data.write(to: scratch, options: [.atomic, .completeFileProtection])
                    defer { try? FileManager.default.removeItem(at: scratch) }
                    guard await AvatarFormatCheck.video(at: scratch, ref: ref) else { throw AvatarLoadRefused() }
                    try disk.write(data, blobIdHex: ref.blobIdHex, kind: .video)
                }
                failures.removeValue(forKey: key)
                revision &+= 1
            } catch {
                // Initials stay; never logged with the blob id.
                failures[key] = now()
                NSLog("[avatar] WARN an avatar could not be loaded; retried after a minute")
            }
        }
    }
}

private struct AvatarLoadRefused: Error {}
