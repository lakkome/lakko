// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 — where decrypted media lives while a chat is open.
///
/// `AVPlayer` and ImageIO read from a file URL, so a decrypted voice message,
/// video note or GIF has to touch the disk. This keeps that bounded:
///
///   * one root directory per open chat, under the app's temporary directory;
///   * one subdirectory per message (keyed by the blob id), so two messages
///     whose senders chose the same `name` can never overwrite each other;
///   * the file name is ``AttachmentBridge/safeFileName(_:fallback:)`` of the
///     SENDER-chosen name, so it cannot walk out of that subdirectory;
///   * ``cleanup()`` removes the whole root when the chat closes, and
///     ``sweepStale(parent:fileManager:)`` removes roots a crash left behind.
///
/// Only authenticated plaintext is ever written: callers hand over the output
/// of `AttachmentBridge.downloadAndDecrypt`, whose AEAD open fails before any
/// byte is returned.
@MainActor
public final class MediaTempFiles {

    nonisolated public static let defaultParent = FileManager.default.temporaryDirectory
        .appendingPathComponent("chat-media", isDirectory: true)

    public let root: URL
    private let fileManager: FileManager
    public private(set) var files: [String: URL] = [:]

    public init(parent: URL = MediaTempFiles.defaultParent, fileManager: FileManager = .default) {
        self.root = parent.appendingPathComponent(UUID().uuidString, isDirectory: true)
        self.fileManager = fileManager
    }

    /// The file already written for `key`, if it still exists.
    public func url(for key: String) -> URL? {
        guard let url = files[key] else { return nil }
        if fileManager.fileExists(atPath: url.path) { return url }
        files.removeValue(forKey: key)
        return nil
    }

    /// Write `data` for `key` and return its URL. Re-writing a key replaces
    /// the previous file.
    @discardableResult
    public func write(_ data: Data, key: String, name: String?, fallback: String) throws -> URL {
        let dir = root.appendingPathComponent(Self.directoryName(for: key), isDirectory: true)
        if let old = files[key] { try? fileManager.removeItem(at: old) }
        try fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(
            AttachmentBridge.safeFileName(name, fallback: fallback),
            isDirectory: false
        )
        try data.write(to: url, options: [.atomic, .completeFileProtection])
        files[key] = url
        return url
    }

    /// Adopt a file that already sits on disk (a recording this device just
    /// made) by moving it under this chat's root.
    @discardableResult
    public func adopt(_ source: URL, key: String, name: String?, fallback: String) throws -> URL {
        let dir = root.appendingPathComponent(Self.directoryName(for: key), isDirectory: true)
        if let old = files[key] { try? fileManager.removeItem(at: old) }
        try fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(
            AttachmentBridge.safeFileName(name, fallback: fallback),
            isDirectory: false
        )
        if fileManager.fileExists(atPath: url.path) { try fileManager.removeItem(at: url) }
        try fileManager.moveItem(at: source, to: url)
        files[key] = url
        return url
    }

    /// File the same bytes under a second key (a sent take, re-keyed from its
    /// local key to its blob id) without copying.
    public func alias(_ key: String, to existingKey: String) {
        if let url = files[existingKey] { files[key] = url }
    }

    public func remove(key: String) {
        guard let url = files.removeValue(forKey: key) else { return }
        // Another key may alias the same file; only delete when none does.
        if !files.values.contains(url) {
            try? fileManager.removeItem(at: url.deletingLastPathComponent())
        }
    }

    /// Remove everything this chat wrote. Idempotent.
    public func cleanup() {
        files.removeAll()
        try? fileManager.removeItem(at: root)
    }

    /// Remove every chat root under `parent` — run at launch, before any chat
    /// opens, so a crash never leaves decrypted media behind for long.
    public static func sweepStale(
        parent: URL = MediaTempFiles.defaultParent,
        fileManager: FileManager = .default
    ) {
        try? fileManager.removeItem(at: parent)
    }

    /// A directory name for `key` made only of `[A-Za-z0-9-]`. Keys are
    /// normally a hex blob id or `local-<n>`; anything else is hex-encoded, so
    /// no key can form a path component such as `..`.
    static func directoryName(for key: String) -> String {
        let allowed = key.unicodeScalars.allSatisfy { s in
            s.isASCII && (CharacterSet.alphanumerics.contains(s) || s == "-")
        }
        if allowed, !key.isEmpty, key.count <= 96 { return key }
        return "k" + AttachmentBridge.hex(Data(key.utf8).prefix(48))
    }
}
