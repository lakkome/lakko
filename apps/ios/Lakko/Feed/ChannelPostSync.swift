// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UIKit

/// Channels join the foreground poll (ADR-0058 section 4): while the app is
/// active, the posts of every channel this device can read are fetched,
/// decrypted and filed into the channel-post log the feed ranks. Mirror of
/// web `feed/ChannelPostSync.tsx`.
///
/// A channel post is not a conversation turn, so the cadence is a minute
/// (chosen), not the chat's long poll; a sync also runs at start and whenever
/// the app becomes active. An app in the background does not sync. When
/// ADR-0055's socket exists its notices replace the timer.
///
/// Only one sync runs at a time, and it is the only writer of the log.
///
/// A post is opened under the seed of its own epoch, never another (ADR-0031,
/// invariant #4). A post whose epoch is newer than every seed this device
/// holds stops the cursor, so it is tried again once the seed arrives; an
/// older one without a seed is past the retained window and is passed over.
@MainActor
final class ChannelPostSync: ObservableObject {

    /// Between foreground syncs (chosen).
    static let intervalSeconds: UInt64 = 60

    /// The log as last read or written; the feed observes it.
    @Published private(set) var log: ChannelPostLogPayload

    private let accountIdB64: String
    private let channels: ChannelsUiState
    private let store: ChannelPostLogStore
    private let client: ChannelsClient
    private var loop: Task<Void, Never>?
    private var running = false
    private var again = false
    private var observers: [NSObjectProtocol] = []

    init(
        accountIdB64: String,
        channels: ChannelsUiState,
        store: ChannelPostLogStore = ChannelPostLogStore(),
        client: ChannelsClient = ChannelsClient()
    ) {
        self.accountIdB64 = accountIdB64
        self.channels = channels
        self.store = store
        self.client = client
        self.log = store.get(accountIdB64: accountIdB64) ?? ChannelPostLogPayload(channels: [:])
    }

    /// Start the timer and the became-active trigger. Idempotent.
    func start() {
        guard loop == nil else { return }
        let center = NotificationCenter.default
        observers.append(center.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in self?.syncNow() }
        })
        loop = Task { [weak self] in
            while !Task.isCancelled {
                // The session root owns this object; once it is gone, so is
                // the loop.
                guard let self else { return }
                self.syncNow()
                try? await Task.sleep(nanoseconds: Self.intervalSeconds * 1_000_000_000)
            }
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers.removeAll()
    }

    /// Run a sync now, or once more after the one in flight.
    func syncNow() {
        guard UIApplication.shared.applicationState != .background else { return }
        if running {
            again = true
            return
        }
        running = true
        Task { [weak self] in
            await self?.syncAll()
            guard let self else { return }
            self.running = false
            if self.again {
                self.again = false
                self.syncNow()
            }
        }
    }

    /// Wait for a sync to finish, for pull to refresh.
    func syncAndWait() async {
        if running {
            again = true
            while running { try? await Task.sleep(nanoseconds: 100_000_000) }
            return
        }
        running = true
        await syncAll()
        running = false
    }

    private struct Readable {
        let idHex: String
        let seedForEpoch: (UInt32) -> Data?
        /// Newest epoch a seed is held for; a post above it waits.
        let newestSeedEpoch: UInt32
    }

    /// Every channel this device can open posts of, and how.
    private func readableChannels() -> [Readable] {
        var out: [Readable] = []
        for c in channels.channels {
            if let keys = channels.keysByChannel[c.id] {
                // A publisher holds the current content seed only, as the
                // channel view does; a post it cannot open under it is passed
                // over.
                let seed = keys.seed
                out.append(Readable(idHex: c.id, seedForEpoch: { _ in seed }, newestSeedEpoch: .max))
                continue
            }
            let map = channels.subscribedSeeds(channelId: c.id)
            guard let newest = map.keys.max() else { continue }
            out.append(Readable(idHex: c.id, seedForEpoch: { map[$0] }, newestSeedEpoch: newest))
        }
        return out
    }

    private func syncAll() async {
        let before = store.get(accountIdB64: accountIdB64) ?? log
        var next = ChannelPostLog.retain(before, keep: Set(channels.channels.map(\.id)))
        var changed = next.channels.count != before.channels.count

        for channel in readableChannels() {
            var metas: [SharedChannelMeta] = []
            let decode: (String) async -> ChannelPostLog.PostOutcome = { payloadB64 in
                guard let epoch = try? ChannelsComposer.postEpoch(postWireB64Url: payloadB64) else {
                    return .skip
                }
                guard let seed = channel.seedForEpoch(epoch) else {
                    return epoch > channel.newestSeedEpoch ? .wait : .skip
                }
                guard let plain = try? ChannelsComposer.verifyAndDecrypt(
                    seed: seed, channelIdHex: channel.idHex, postWireB64Url: payloadB64)
                else { return .skip }
                switch ChannelMetaRecord.classify(plain) {
                case .text(let text): return .text(text)
                case .meta(let meta):
                    metas.append(meta)
                    return .skip
                case .malformed: return .skip
                }
            }
            let slot = next.channels[channel.idHex]
            do {
                let client = self.client
                let id = channel.idHex
                let updated = try await ChannelPostLog.syncSlot(
                    slot,
                    fetchPage: { after, limit in
                        let res = try await client.fetchPosts(channelIdHex: id, after: after, limit: limit)
                        return ChannelPostLog.Page(posts: res.posts.map {
                            (cursor: $0.cursor,
                             serverTsMs: Int64(clamping: $0.serverTsMs),
                             payloadB64: $0.payloadB64)
                        })
                    },
                    decode: decode,
                    nowMs: Int64(Date().timeIntervalSince1970 * 1000))
                if updated != slot {
                    var channelsMap = next.channels
                    channelsMap[channel.idHex] = updated
                    next = ChannelPostLogPayload(channels: channelsMap)
                    changed = true
                }
            } catch {
                // A channel the server does not answer for this time; the
                // next sync tries again from the same cursor.
                NSLog("[feed] channel post sync failed")
            }
            // A metadata record read here names the channel in the feed and
            // the list without opening it (ADR-0051).
            for meta in metas {
                _ = channels.recordSharedMeta(channelId: channel.idHex, meta: meta)
            }
        }

        guard changed else { return }
        store.put(accountIdB64: accountIdB64, payload: next)
        log = next
    }
}
