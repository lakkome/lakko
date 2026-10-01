// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The signed translation update kept from an earlier run: the raw bytes as
/// downloaded (re-verified at every launch) plus the version and signing
/// time recorded when they were accepted.
struct StoredTranslationBundle: Equatable {
    var bytes: Data
    var version: UInt64
    var signedAtMillis: Int64
}

/// Where the accepted bundle is kept.
protocol TranslationBundleStore: AnyObject {
    func load() -> StoredTranslationBundle?
    func save(_ bundle: StoredTranslationBundle) throws
    func delete()
}

/// The app-private store: two files under Application Support, excluded
/// from backup. Not the App Group container: the notification extension
/// never reads the bundle, it gets the rendered words through
/// ``SharedStorage/setNotificationWords(_:)`` whenever the catalog loads.
/// The bundle is a public signed artifact, so no encryption is needed.
final class FileTranslationBundleStore: TranslationBundleStore {
    private let dir: URL?

    init(dir: URL? = FileTranslationBundleStore.defaultDir()) {
        self.dir = dir
    }

    static func defaultDir() -> URL? {
        try? FileManager.default
            .url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            .appendingPathComponent("lakko-translations", isDirectory: true)
    }

    private var bytesUrl: URL? { dir?.appendingPathComponent("bundle.bin") }
    private var metaUrl: URL? { dir?.appendingPathComponent("bundle.json") }

    private struct Meta: Codable {
        var version: String
        var signedAtMillis: Int64
    }

    func load() -> StoredTranslationBundle? {
        guard let bytesUrl, let metaUrl,
              let bytes = try? Data(contentsOf: bytesUrl),
              let metaData = try? Data(contentsOf: metaUrl),
              let meta = try? JSONDecoder().decode(Meta.self, from: metaData),
              let version = UInt64(meta.version) else {
            return nil
        }
        return StoredTranslationBundle(bytes: bytes, version: version, signedAtMillis: meta.signedAtMillis)
    }

    func save(_ bundle: StoredTranslationBundle) throws {
        guard var dir, let bytesUrl, let metaUrl else {
            throw CocoaError(.fileNoSuchFile)
        }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try dir.setResourceValues(values)
        let meta = Meta(version: String(bundle.version), signedAtMillis: bundle.signedAtMillis)
        // Bytes first: a crash between the two writes leaves new bytes with
        // the old record, which the launch rule discards (version mismatch).
        try bundle.bytes.write(to: bytesUrl, options: .atomic)
        try JSONEncoder().encode(meta).write(to: metaUrl, options: .atomic)
    }

    func delete() {
        guard let dir else { return }
        try? FileManager.default.removeItem(at: dir)
    }
}
