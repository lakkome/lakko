// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// One saved feed: a local id and the rule's canonical bytes.
struct SavedFeed: Equatable {
    /// Local id, random, stable across edits.
    let id: String
    /// The rule's canonical encoding (`encodeFeedRule`).
    let rule: Data
}

/// The person's own feeds on this device (ADR-0058 section 6). Mirror of web
/// `feed/feed-store.ts`: each a rule in the core's canonical encoding, sealed
/// per account under a device-bound key. They do not follow the person to
/// other devices.
///
/// ```text
///   Application Support/lakko-feed-state/<accountIdB64>.bin
///     12-byte IV || ciphertext || 16-byte tag (AES-256-GCM, Keychain wrap key)
///   plaintext  { "version": 1, "feeds": [{ "id", "rule": <base64url> }] }
/// ```
///
/// The codec is lenient per entry: a feed whose id or bytes are malformed is
/// dropped alone. Whether the bytes are a valid rule is the core's to say
/// when the feed is opened.
struct FeedState: Equatable {
    static let version = 1
    /// Most feeds kept (chosen): a switcher row, not a library.
    static let maxFeeds = 20

    var feeds: [SavedFeed]

    init(feeds: [SavedFeed] = []) {
        self.feeds = feeds
    }

    /// Add or replace a feed; a new one goes to the end.
    func upserting(_ feed: SavedFeed) -> FeedState {
        var feeds = self.feeds
        if let i = feeds.firstIndex(where: { $0.id == feed.id }) {
            feeds[i] = feed
            return FeedState(feeds: feeds)
        }
        feeds.append(feed)
        return FeedState(feeds: Array(feeds.prefix(Self.maxFeeds)))
    }

    func removing(id: String) -> FeedState {
        FeedState(feeds: feeds.filter { $0.id != id })
    }

    /// A fresh local feed id.
    static func newId() -> String {
        var bytes = [UInt8](repeating: 0, count: 12)
        for i in bytes.indices { bytes[i] = UInt8.random(in: 0...255) }
        return FeedDraft.base64UrlNoPad(Data(bytes))
    }

    // MARK: - Codec

    private struct Wire: Codable {
        struct Entry: Codable {
            let id: String?
            let rule: String?
        }
        let version: Int?
        let feeds: [Entry]?
    }

    static func decode(_ data: Data) -> FeedState {
        guard let wire = try? JSONDecoder().decode(Wire.self, from: data),
              wire.version == version, let entries = wire.feeds
        else { return FeedState() }
        var feeds: [SavedFeed] = []
        for e in entries {
            guard let id = e.id, isValidId(id), let rule = e.rule,
                  !feeds.contains(where: { $0.id == id }),
                  let bytes = FeedDraft.bytes(fromFragment: rule)
            else { continue }
            feeds.append(SavedFeed(id: id, rule: bytes))
            if feeds.count >= maxFeeds { break }
        }
        return FeedState(feeds: feeds)
    }

    func encode() -> Data {
        let wire = Wire(
            version: Self.version,
            feeds: feeds.map { .init(id: $0.id, rule: FeedDraft.base64UrlNoPad($0.rule)) })
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return (try? encoder.encode(wire)) ?? Data()
    }

    static func isValidId(_ id: String) -> Bool {
        (8...32).contains(id.utf8.count)
            && id.utf8.allSatisfy {
                (0x30...0x39).contains($0) || (0x41...0x5a).contains($0)
                    || (0x61...0x7a).contains($0) || $0 == 0x2d || $0 == 0x5f
            }
    }
}

/// Sealed per-account file behind ``FeedState``.
final class FeedStore {

    static let wrapKeyService: String = "me.lakko.feed-state-wrap.v1"
    private static let wrapKeyAccount: String = "wrap-key"
    private static let subdir: String = "lakko-feed-state"

    init() {}

    func load(accountIdB64: String) -> FeedState {
        guard let url = fileURL(accountIdB64: accountIdB64),
              let combined = try? Data(contentsOf: url),
              let keyBytes = KeychainWrapKey.read(
                service: Self.wrapKeyService, account: Self.wrapKeyAccount),
              let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: SymmetricKey(data: keyBytes))
        else { return FeedState() }
        return FeedState.decode(plaintext)
    }

    /// Throws when the file cannot be written, so Save can say so.
    func save(accountIdB64: String, state: FeedState) throws {
        guard let url = fileURL(accountIdB64: accountIdB64) else {
            throw CocoaError(.fileWriteUnknown)
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        let combined = try AES.GCM.seal(state.encode(), using: wrapKey).combined
        guard let combined else { throw CocoaError(.fileWriteUnknown) }
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(dir)
        try combined.write(to: url, options: .atomic)
        SharedStorage.excludeFromBackup(url)
    }

    /// Erase the saved feeds of an account. Called from `forgetDevice`.
    func delete(accountIdB64: String) {
        if let url = fileURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: url)
        }
    }

    private func fileURL(accountIdB64: String) -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true)
        else { return nil }
        return appSupport
            .appendingPathComponent(Self.subdir)
            .appendingPathComponent("\(accountIdB64.replacingOccurrences(of: "/", with: "_")).bin")
    }
}
