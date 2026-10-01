// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// The channel-post log (ADR-0061 section 3): the posts of subscribed and own
/// channels this device has decrypted, kept so the feed can rank them without
/// opening each channel. Mirror of web `feed/channel-post-log.ts`.
///
/// The codec, the merge and the per-channel sync are pure here with their
/// network and decrypt dependencies injected, so the unit tests run them with
/// no Keychain, no network and no core. ``ChannelPostLogStore`` seals the
/// payload; ``ChannelPostSync`` drives the sync while the app is in front.
///
/// Bounds, both chosen: posts older than the server's 30-day retention are
/// dropped (the log never keeps what the server could no longer serve), and
/// at most ``cap`` posts are kept per channel, newest kept.
enum ChannelPostLog {
    /// Payload schema version this client writes.
    static let version = 1
    /// Most posts kept per channel (chosen, ADR-0061 open question 1).
    static let cap = 500
    /// The server's post retention; older posts are dropped from the log.
    static let retentionMs: Int64 = 30 * 86_400_000
    /// Posts asked for per request.
    static let page = 100
    /// Pages fetched per channel per sync, so one busy channel cannot hold the
    /// loop; the rest arrives on the next sync.
    static let maxPages = 5

    /// Fold newly decrypted posts into a slot: de-duplicated by cursor (a
    /// later copy wins), ascending, trimmed to the retention window and the
    /// cap. The cursor never moves backwards.
    static func merge(
        _ slot: ChannelPostSlot?,
        fresh: [PersistedChannelPost],
        cursor: UInt64,
        nowMs: Int64
    ) -> ChannelPostSlot {
        var byCursor: [UInt64: PersistedChannelPost] = [:]
        for p in slot?.posts ?? [] { byCursor[p.cursor] = p }
        for p in fresh { byCursor[p.cursor] = p }
        let oldest = nowMs - retentionMs
        let posts = byCursor.values
            .filter { $0.serverTsMs >= oldest }
            .sorted { $0.cursor < $1.cursor }
            .suffix(cap)
        return ChannelPostSlot(cursor: max(slot?.cursor ?? 0, cursor), posts: Array(posts))
    }

    /// Drop the slots of channels this device no longer holds.
    static func retain(_ payload: ChannelPostLogPayload, keep: Set<String>) -> ChannelPostLogPayload {
        ChannelPostLogPayload(channels: payload.channels.filter { keep.contains($0.key) })
    }

    // MARK: - Sync

    /// What one post decoded to, as far as the log cares.
    enum PostOutcome: Equatable {
        /// A text post: kept.
        case text(String)
        /// Read and not a feed item (a metadata record, a malformed record, a
        /// post that failed to open under its epoch's seed): passed over.
        case skip
        /// No seed for the post's epoch and one may still arrive (the epoch
        /// is newer than every seed this device holds): the cursor stops
        /// before it so the next sync tries again.
        case wait
    }

    struct Page {
        let posts: [(cursor: UInt64, serverTsMs: Int64, payloadB64: String)]
    }

    /// Bring one channel's slot up to date: fetch after its cursor, decode
    /// each post, keep the text ones. Stops at the first post that must wait
    /// for a seed, at an empty page, or after ``maxPages`` pages. A fetch
    /// failure throws; the slot the caller holds is unchanged.
    static func syncSlot(
        _ slot: ChannelPostSlot?,
        fetchPage: (_ after: UInt64, _ limit: Int) async throws -> Page,
        decode: (_ payloadB64: String) async -> PostOutcome,
        nowMs: Int64
    ) async throws -> ChannelPostSlot {
        var cursor = slot?.cursor ?? 0
        var fresh: [PersistedChannelPost] = []
        for _ in 0..<maxPages {
            let res = try await fetchPage(cursor, page)
            if res.posts.isEmpty { break }
            var blocked = false
            for p in res.posts.sorted(by: { $0.cursor < $1.cursor }) {
                if p.cursor <= cursor { continue }
                let outcome = await decode(p.payloadB64)
                if outcome == .wait {
                    blocked = true
                    break
                }
                if case .text(let text) = outcome {
                    fresh.append(
                        PersistedChannelPost(cursor: p.cursor, serverTsMs: p.serverTsMs, text: text))
                }
                cursor = p.cursor
            }
            if blocked || res.posts.count < page { break }
        }
        return merge(slot, fresh: fresh, cursor: cursor, nowMs: nowMs)
    }
}

/// One decrypted text post.
struct PersistedChannelPost: Codable, Equatable {
    /// The server's per-channel cursor, the post's id in the feed.
    let cursor: UInt64
    /// Server receive time, unix ms.
    let serverTsMs: Int64
    let text: String
}

/// One channel's slice of the log.
struct ChannelPostSlot: Codable, Equatable {
    /// Every post at or below this cursor has been looked at.
    let cursor: UInt64
    /// Ascending by cursor.
    let posts: [PersistedChannelPost]
}

/// The whole log for one account.
struct ChannelPostLogPayload: Codable, Equatable {
    let version: Int
    let channels: [String: ChannelPostSlot]

    init(channels: [String: ChannelPostSlot]) {
        self.version = ChannelPostLog.version
        self.channels = channels
    }

    enum CodingKeys: String, CodingKey {
        case version, channels
    }

    /// An unknown version throws (the caller starts empty); a malformed
    /// channel id is dropped alone.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let version = try c.decode(Int.self, forKey: .version)
        guard version == ChannelPostLog.version else {
            throw DecodingError.dataCorruptedError(
                forKey: .version, in: c, debugDescription: "unknown channel-post log version")
        }
        self.version = version
        let raw = try c.decode([String: ChannelPostSlot].self, forKey: .channels)
        var channels: [String: ChannelPostSlot] = [:]
        for (id, slot) in raw where Self.isChannelId(id) {
            channels[id] = ChannelPostSlot(
                cursor: slot.cursor,
                posts: slot.posts.filter { $0.cursor > 0 }.sorted { $0.cursor < $1.cursor })
        }
        self.channels = channels
    }

    static func isChannelId(_ s: String) -> Bool {
        s.utf8.count == 32 && s.utf8.allSatisfy { (0x30...0x39).contains($0) || (0x61...0x66).contains($0) }
    }
}

/// Sealed per-account store for ``ChannelPostLogPayload``: the
/// ``ChannelLogStore`` posture (AES-256-GCM under a Keychain-resident wrap
/// key, device-bound, excluded from backup), one file per account under
/// `Application Support/lakko-channel-post-logs/`. Only the foreground app
/// reads it, so it is not in the App Group.
final class ChannelPostLogStore {

    static let wrapKeyService: String = "me.lakko.channel-post-log-wrap.v1"
    private static let wrapKeyAccount: String = "wrap-key"
    private static let subdir: String = "lakko-channel-post-logs"

    init() {}

    func put(accountIdB64: String, payload: ChannelPostLogPayload) {
        guard let url = fileURL(accountIdB64: accountIdB64) else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        guard let plaintext = try? encoder.encode(payload) else { return }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try? AES.GCM.seal(plaintext, using: wrapKey).combined else { return }
        let dir = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(dir)
        do {
            try combined.write(to: url, options: .atomic)
            SharedStorage.excludeFromBackup(url)
        } catch {
            NSLog("[feed] channel post log write failed")
        }
    }

    /// `nil` when absent, undecryptable or of an unknown version.
    func get(accountIdB64: String) -> ChannelPostLogPayload? {
        guard let url = fileURL(accountIdB64: accountIdB64),
              let combined = try? Data(contentsOf: url),
              let keyBytes = KeychainWrapKey.read(
                service: Self.wrapKeyService, account: Self.wrapKeyAccount)
        else { return nil }
        guard let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: SymmetricKey(data: keyBytes))
        else { return nil }
        return try? JSONDecoder().decode(ChannelPostLogPayload.self, from: plaintext)
    }

    /// Erase the log for an account. Called from `forgetDevice`.
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
