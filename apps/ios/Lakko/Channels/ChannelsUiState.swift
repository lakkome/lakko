// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Local channels state (Phase 4 G4-2 iOS). Mirror of
/// `apps/web/src/state/channels-state.ts` and
/// `apps/android/.../channels/ChannelsUiState.kt`
/// (when it ships).
///
/// Holds the user's channel list, per-channel publisher
/// keys, server-cached posts + epochs. Display names +
/// descriptions are **local-only** — never sent to the
/// server.
@MainActor
public final class ChannelsUiState: ObservableObject {

    /// User's local channel list, newest-first.
    @Published public private(set) var channels: [ChannelHandle] = []

    /// Per-channel post cache (filled by
    /// ``setPosts(channelId:posts:)``).
    @Published public private(set) var postsByChannel: [String: [PostView]] = [:]

    /// Per-channel server epoch cache.
    @Published public private(set) var epochByChannel: [String: UInt32] = [:]

    /// Per-channel publisher keys (local-only).
    @Published public private(set) var keysByChannel: [String: ChannelPublisherKeys] = [:]

    /// Per-channel monotone publish counter. Reset on
    /// ``setKeys(channelId:keys:)`` — fresh keys imply a
    /// fresh `(epoch, sender)` pair, which starts a fresh
    /// counter sequence per the `lakko-channels`
    /// invariants.
    @Published public private(set) var counterByChannel: [String: UInt64] = [:]

    /// Per-channel subscriber-side X25519 keypair (Phase 4b
    /// G4-3c). The secret half is persisted in the hardware-backed
    /// Keychain (``ChannelSubscriberKeyStore``, G-29) so a relaunch
    /// does not strand the user without the secret needed to open
    /// future seed envelopes — never `UserDefaults` plaintext
    /// (invariant #7).
    @Published public private(set)
    var subscriberKeysByChannel: [String: ChannelSubscriberKeys] = [:]

    /// Per-channel recovered content-key seeds, keyed by
    /// epoch (ADR-0031). A subscriber retains the most-recent
    /// N epochs' seeds so it can decrypt a post made under a
    /// rotation it missed — each `ChannelPost` carries its
    /// `epoch_id`, and the reader selects the seed matching
    /// that epoch. In-memory only — never persisted, never
    /// sent off-device (same posture as the single-seed cache
    /// it replaces). UI shows a "waiting for seed delivery"
    /// state until the map has an entry for the channel.
    @Published public private(set)
    var subscribedSeedsByChannel: [String: [UInt32: Data]] = [:]

    /// **Legacy** `UserDefaults` key — the pre-G-29 plaintext
    /// storage location, now read only by ``loadSubscriberKeys()`` for
    /// the one-time migration into ``ChannelSubscriberKeyStore`` (after
    /// which the plaintext copy is scrubbed). New writes go to the
    /// Keychain, never here.
    public static let subscriberKeysStorageKey =
        "lakko.channels.subscriber-keys.v1"

    public init() {
        // Restore persisted subscriber keypairs so a fresh
        // launch can poll for envelopes without the user
        // re-subscribing manually.
        self.subscriberKeysByChannel = Self.loadSubscriberKeys()
    }

    // MARK: - Mutations

    public func addChannel(_ handle: ChannelHandle) {
        // Id-idempotent: re-adding refreshes the local
        // metadata rather than appending a duplicate.
        var without = channels.filter { $0.id != handle.id }
        without.insert(handle, at: 0)
        channels = without
    }

    public func removeChannel(_ channelId: String) {
        channels.removeAll { $0.id == channelId }
        postsByChannel[channelId] = nil
        epochByChannel[channelId] = nil
        keysByChannel[channelId] = nil
        counterByChannel[channelId] = nil
    }

    public func updateLocalMeta(
        channelId: String,
        localName: String? = nil,
        localDescription: String? = nil
    ) {
        guard let idx = channels.firstIndex(where: { $0.id == channelId })
        else { return }
        let old = channels[idx]
        channels[idx] = ChannelHandle(
            id: old.id,
            localName: localName ?? old.localName,
            localDescription: localDescription ?? old.localDescription,
            addedAtMs: old.addedAtMs
        )
    }

    public func setPosts(channelId: String, posts: [PostView]) {
        postsByChannel[channelId] = posts
    }

    public func setEpoch(channelId: String, epoch: UInt32) {
        epochByChannel[channelId] = epoch
    }

    /// Install fresh publisher keys; resets counter to 0.
    public func setKeys(channelId: String, keys: ChannelPublisherKeys) {
        keysByChannel[channelId] = keys
        counterByChannel[channelId] = 0
    }

    /// Atomically read-and-bump the per-channel publish
    /// counter. Returns the value the NEXT post should
    /// carry; the state is updated to one past that.
    public func nextCounter(channelId: String) -> UInt64 {
        let current = counterByChannel[channelId] ?? 0
        counterByChannel[channelId] = current + 1
        return current
    }

    /// Install / replace the subscriber-side X25519
    /// keypair the device generated when joining
    /// `channelId` as a reader. Persists to
    /// `UserDefaults` synchronously.
    public func setSubscriberKeys(
        channelId: String,
        keys: ChannelSubscriberKeys
    ) {
        subscriberKeysByChannel[channelId] = keys
        persistSubscriberKeys()
    }

    /// Forget the subscriber-side keypair for
    /// `channelId` — used on un-follow. Clears the cached
    /// recovered seed map too.
    public func clearSubscriberKeys(_ channelId: String) {
        subscriberKeysByChannel[channelId] = nil
        subscribedSeedsByChannel[channelId] = nil
        persistSubscriberKeys()
    }

    /// Cache the recovered content-key seed for one epoch
    /// after the subscriber successfully opened that epoch's
    /// seed envelope (ADR-0031). Merges into the channel's
    /// `epoch -> seed` map so multiple retained epochs coexist.
    public func setSubscribedSeed(channelId: String, epoch: UInt32, seed: Data) {
        var byEpoch = subscribedSeedsByChannel[channelId] ?? [:]
        byEpoch[epoch] = seed
        subscribedSeedsByChannel[channelId] = byEpoch
    }

    /// The recovered `epoch -> seed` map for `channelId`
    /// (empty when no envelope has been opened yet).
    public func subscribedSeeds(channelId: String) -> [UInt32: Data] {
        subscribedSeedsByChannel[channelId] ?? [:]
    }

    public func reset() {
        channels = []
        postsByChannel = [:]
        epochByChannel = [:]
        keysByChannel = [:]
        counterByChannel = [:]
        subscriberKeysByChannel = [:]
        subscribedSeedsByChannel = [:]
        persistSubscriberKeys()
    }

    // MARK: - Subscriber-keypair persistence

    /// One row in the persisted `UserDefaults` blob.
    private struct PersistedSubscriberKey: Codable {
        let channelId: String
        /// Hex-encoded 32-byte X25519 secret.
        let secretHex: String
        /// Hex-encoded 32-byte X25519 public.
        let publicHex: String
    }

    private func persistSubscriberKeys() {
        // G-29 — the X25519 secret half is hardware-backed-Keychain material,
        // not `UserDefaults` plaintext (invariant #7). An empty set wipes the
        // item (un-follow-all / reset / logout).
        if subscriberKeysByChannel.isEmpty {
            ChannelSubscriberKeyStore.delete()
            return
        }
        let rows: [PersistedSubscriberKey] =
            subscriberKeysByChannel.map { (channelId, k) in
                PersistedSubscriberKey(
                    channelId: channelId,
                    secretHex: ChannelsComposer.bytesToHex(k.secret),
                    publicHex: ChannelsComposer.bytesToHex(k.publicKey)
                )
            }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        ChannelSubscriberKeyStore.save(data)
    }

    private static func loadSubscriberKeys() -> [String: ChannelSubscriberKeys] {
        var blob = ChannelSubscriberKeyStore.load()
        if blob == nil {
            // One-time migration: an install that persisted the keys to the
            // legacy `UserDefaults` plaintext key before G-29 — move the blob
            // into the Keychain and scrub the plaintext copy.
            if let legacy = UserDefaults.standard.data(
                forKey: subscriberKeysStorageKey
            ) {
                ChannelSubscriberKeyStore.save(legacy)
                UserDefaults.standard.removeObject(
                    forKey: subscriberKeysStorageKey
                )
                blob = legacy
            }
        }
        guard
            let data = blob,
            let rows = try? JSONDecoder().decode(
                [PersistedSubscriberKey].self,
                from: data
            )
        else {
            return [:]
        }
        var out: [String: ChannelSubscriberKeys] = [:]
        for row in rows {
            guard let secret = Data(hexString: row.secretHex),
                  let pub = Data(hexString: row.publicHex)
            else { continue }
            out[row.channelId] = ChannelSubscriberKeys(
                secret: secret,
                publicKey: pub
            )
        }
        return out
    }
}

private extension Data {
    init?(hexString: String) {
        let chars = Array(hexString)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        var i = 0
        while i < chars.count {
            guard let byte = UInt8(String(chars[i...(i + 1)]), radix: 16)
            else { return nil }
            out.append(byte)
            i += 2
        }
        self = out
    }
}

/// One channel as the local UI sees it. Display name +
/// description are **local-only** — never sent to the
/// server.
public struct ChannelHandle: Equatable, Sendable, Identifiable {
    /// 32-char lowercase hex (the wire form). Same string
    /// the `services/channels` REST URLs accept.
    public let id: String
    public let localName: String
    public let localDescription: String
    /// Unix milliseconds — when the user added this
    /// channel to their local list.
    public let addedAtMs: UInt64

    public init(
        id: String,
        localName: String,
        localDescription: String,
        addedAtMs: UInt64
    ) {
        self.id = id
        self.localName = localName
        self.localDescription = localDescription
        self.addedAtMs = addedAtMs
    }
}
