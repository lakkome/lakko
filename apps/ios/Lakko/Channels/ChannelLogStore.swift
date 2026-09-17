// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// Persistent per-account channel log on iOS: the local channel list and the
/// subscriber-side recovered content-key seeds.
///
/// Before this store both lived only in ``ChannelsUiState`` (seen live on
/// 2026-09-17): every relaunch came up with an empty Channels tab, so a
/// subscriber lost the channels it had joined and a creator lost the channel
/// it had made, even though the Keychain still held the publisher signing
/// seeds, the publish counters and the subscriber keypairs. The channel
/// `@handle` is the only way back to a channel id (discovery is a one-way
/// OPRF), so the handle is persisted with the row.
///
/// ## Threat model
///
/// The channel list is relationship metadata (which channels this account
/// reads or publishes) and the subscribed seeds are the channels' content
/// keys — the same class as the plaintext message log. The file is sealed
/// with AES-256-GCM under a Keychain-resident wrap key, the
/// ``MessageLogStore`` posture: a dedicated alias the OS keeps hardware-
/// isolated when the device supports it, device-bound, no iCloud sync.
///
/// ## On-disk format
///
/// ```text
///   <App Group container>/lakko-channel-logs/
///     <accountIdB64>.bin     12-byte IV || ciphertext || 16-byte tag
/// ```
///
/// falling back to `Application Support/lakko-channel-logs/` when the App
/// Group is not provisioned. Plaintext is the JSON-encoded
/// ``ChannelLogPayload``; `version` lets a later schema bump refuse rather
/// than misread an older blob.
///
/// ## Mutation cadence
///
/// ``ChannelsUiState`` writes through on every mutation of the channel list
/// or the seed map. The blob is small (a few KB) so atomic-replace I/O per
/// mutation is fine without batching.
public final class ChannelLogStore {

    /// Keychain service tag for the AES wrap key. Versioned so a future
    /// rotation has somewhere to go without colliding with v1 entries.
    public static let wrapKeyService: String = "me.lakko.channel-log-wrap.v1"

    /// One wrap key per app installation (not per Lakko account); per-account
    /// isolation comes from the file path, as for ``MessageLogStore``.
    private static let wrapKeyAccount: String = "wrap-key"

    /// Container subdirectory holding the per-account envelopes.
    private static let subdir: String = "lakko-channel-logs"

    public init() {}

    // MARK: - Public API

    /// Persist the channel log for `accountIdB64`, replacing any prior
    /// envelope. Best-effort: a Keychain or file-system failure is a no-op
    /// here and the next mutation writes again.
    public func put(accountIdB64: String, payload: ChannelLogPayload) {
        guard let target = writeTarget(accountIdB64: accountIdB64) else { return }
        let plaintext: Data
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            plaintext = try encoder.encode(payload)
        } catch {
            return
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try? AES.GCM.seal(plaintext, using: wrapKey).combined else { return }
        do {
            try FileManager.default.createDirectory(
                at: target.dir,
                withIntermediateDirectories: true
            )
            SharedStorage.excludeFromBackup(target.dir)
        } catch {
            // best-effort
        }
        do {
            try combined.write(to: target.url, options: .atomic)
            SharedStorage.excludeFromBackup(target.url)
        } catch {
            // best-effort
        }
    }

    /// Read and unwrap the log for `accountIdB64`. `nil` when no envelope
    /// exists, the wrap key is unavailable, the AEAD open fails, or the
    /// payload does not decode — the caller starts with an empty list, never
    /// a guessed one.
    public func get(accountIdB64: String) -> ChannelLogPayload? {
        let candidates = readCandidates(accountIdB64: accountIdB64)
        guard !candidates.isEmpty else { return nil }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            NSLog("channel log: wrap key unavailable")
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        return SharedStorage.firstReadable(candidates) { url -> ChannelLogPayload? in
            guard let combined = try? Data(contentsOf: url) else {
                NSLog("channel log: unreadable file")
                return nil
            }
            let plaintext: Data
            do {
                let sealed = try AES.GCM.SealedBox(combined: combined)
                plaintext = try AES.GCM.open(sealed, using: wrapKey)
            } catch {
                NSLog("channel log: open failed: \(error)")
                return nil
            }
            do {
                return try JSONDecoder().decode(ChannelLogPayload.self, from: plaintext)
            } catch {
                NSLog("channel log: decode failed: \(error)")
                return nil
            }
        }
    }

    /// Erase the log for `accountIdB64` from both locations. No-op when
    /// absent. Called from `forgetDevice` and from ``ChannelsUiState/reset()``.
    public func delete(accountIdB64: String) {
        if let n = containerURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: n)
        }
        if let o = appSupportURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: o)
        }
    }

    // MARK: - File layout

    private func fileName(_ accountIdB64: String) -> String {
        "\(accountIdB64.replacingOccurrences(of: "/", with: "_")).bin"
    }

    private func containerDir() -> URL? {
        SharedStorage.containerURL()?.appendingPathComponent(Self.subdir)
    }

    private func containerURL(accountIdB64: String) -> URL? {
        containerDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    private func appSupportDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent(Self.subdir)
    }

    private func appSupportURL(accountIdB64: String) -> URL? {
        appSupportDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    /// Ordered read candidates (preferred first), each included only when it
    /// exists on disk, so an unopenable preferred copy falls through to the
    /// other rather than shadowing it.
    private func readCandidates(accountIdB64: String) -> [URL] {
        let fm = FileManager.default
        var urls: [URL] = []
        if let n = containerURL(accountIdB64: accountIdB64), fm.fileExists(atPath: n.path) {
            urls.append(n)
        }
        if let o = appSupportURL(accountIdB64: accountIdB64), fm.fileExists(atPath: o.path) {
            urls.append(o)
        }
        return urls
    }

    private func writeTarget(accountIdB64: String) -> (dir: URL, url: URL)? {
        if let dir = containerDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        if let dir = appSupportDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        return nil
    }
}

// MARK: - Payload

/// Schema-versioned plaintext of one account's channel log. Pure `Codable`,
/// FFI-free, so the codec is unit-testable without the app running.
public struct ChannelLogPayload: Codable, Equatable {
    /// The one schema this reader accepts; a blob with another version does
    /// not decode (the caller falls back to an empty list).
    public static let currentVersion: Int = 1

    public let version: Int
    /// The local channel list in display order (newest-first, as
    /// ``ChannelsUiState/channels`` keeps it).
    public var channels: [PersistedChannel]
    /// Subscriber-side recovered content-key seeds, one row per
    /// `(channel, epoch)`; sorted on encode so the ciphertext for the same
    /// state is stable.
    public var subscribedSeeds: [PersistedSubscribedSeed]

    public init(channels: [PersistedChannel], subscribedSeeds: [PersistedSubscribedSeed]) {
        self.version = Self.currentVersion
        self.channels = channels
        self.subscribedSeeds = subscribedSeeds.sorted {
            ($0.channelId, $0.epoch) < ($1.channelId, $1.epoch)
        }
    }

    public static let empty = ChannelLogPayload(channels: [], subscribedSeeds: [])

    enum CodingKeys: String, CodingKey {
        case version, channels, subscribedSeeds
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let version = try c.decode(Int.self, forKey: .version)
        guard version == Self.currentVersion else {
            throw DecodingError.dataCorruptedError(
                forKey: .version,
                in: c,
                debugDescription: "unsupported channel log version \(version)"
            )
        }
        self.version = version
        self.channels = try c.decode([PersistedChannel].self, forKey: .channels)
        self.subscribedSeeds = try c.decode([PersistedSubscribedSeed].self, forKey: .subscribedSeeds)
    }

    // MARK: Bridges to the in-memory shapes

    /// Project the live state into the payload.
    public init(channels: [ChannelHandle], subscribedSeedsByChannel: [String: [UInt32: Data]]) {
        var rows: [PersistedSubscribedSeed] = []
        for (channelId, byEpoch) in subscribedSeedsByChannel {
            for (epoch, seed) in byEpoch {
                rows.append(PersistedSubscribedSeed(
                    channelId: channelId,
                    epoch: epoch,
                    seedB64: seed.base64EncodedString()
                ))
            }
        }
        self.init(
            channels: channels.map(PersistedChannel.init(handle:)),
            subscribedSeeds: rows
        )
    }

    /// The channel list as ``ChannelsUiState`` holds it, in stored order.
    public var channelHandles: [ChannelHandle] {
        channels.map(\.channelHandle)
    }

    /// The `channelId -> epoch -> seed` map as ``ChannelsUiState`` holds it.
    /// A row whose seed is not valid base64 or not 32 bytes is skipped: a
    /// wrong-length seed would only fail the AEAD later, and skipping it
    /// keeps the reader on its "waiting for seed delivery" path instead.
    public var subscribedSeedsByChannel: [String: [UInt32: Data]] {
        var out: [String: [UInt32: Data]] = [:]
        for row in subscribedSeeds {
            guard let seed = Data(base64Encoded: row.seedB64), seed.count == 32 else { continue }
            out[row.channelId, default: [:]][row.epoch] = seed
        }
        return out
    }
}

/// JSON projection of a ``ChannelHandle``.
public struct PersistedChannel: Codable, Equatable {
    public let id: String
    public let localName: String
    public let localDescription: String
    public let addedAtMs: UInt64
    public let handle: String
    /// ADR-0051 §4 — the newest shared metadata record's fields. Optional in
    /// the JSON so a blob written before the record existed decodes with no
    /// shared name (the row falls back to the local name / handle) — no
    /// schema bump needed.
    public let sharedName: String?
    public let sharedDescription: String?
    public let sharedUpdatedAtMs: UInt64?

    public init(
        id: String,
        localName: String,
        localDescription: String,
        addedAtMs: UInt64,
        handle: String,
        sharedName: String? = nil,
        sharedDescription: String? = nil,
        sharedUpdatedAtMs: UInt64? = nil
    ) {
        self.id = id
        self.localName = localName
        self.localDescription = localDescription
        self.addedAtMs = addedAtMs
        self.handle = handle
        self.sharedName = sharedName
        self.sharedDescription = sharedDescription
        self.sharedUpdatedAtMs = sharedUpdatedAtMs
    }

    public init(handle h: ChannelHandle) {
        self.init(
            id: h.id,
            localName: h.localName,
            localDescription: h.localDescription,
            addedAtMs: h.addedAtMs,
            handle: h.handle,
            sharedName: h.sharedUpdatedAtMs > 0 ? h.sharedName : nil,
            sharedDescription: h.sharedUpdatedAtMs > 0 ? h.sharedDescription : nil,
            sharedUpdatedAtMs: h.sharedUpdatedAtMs > 0 ? h.sharedUpdatedAtMs : nil
        )
    }

    public var channelHandle: ChannelHandle {
        ChannelHandle(
            id: id,
            localName: localName,
            localDescription: localDescription,
            addedAtMs: addedAtMs,
            handle: handle,
            sharedName: sharedName ?? "",
            sharedDescription: sharedDescription ?? "",
            sharedUpdatedAtMs: sharedUpdatedAtMs ?? 0
        )
    }
}

/// One subscriber-side recovered seed: the channel, the epoch the seed
/// belongs to (ADR-0031 keeps several), and the 32-byte seed as base64.
public struct PersistedSubscribedSeed: Codable, Equatable {
    public let channelId: String
    public let epoch: UInt32
    public let seedB64: String

    public init(channelId: String, epoch: UInt32, seedB64: String) {
        self.channelId = channelId
        self.epoch = epoch
        self.seedB64 = seedB64
    }
}
