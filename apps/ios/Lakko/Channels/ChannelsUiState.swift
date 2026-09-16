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

    /// Per-channel subscriber count as last read from the server (the
    /// owner's manage screen and the list refresh fill it). `nil` until
    /// known; shown on the channel row, never persisted.
    @Published public private(set) var subscriberCountByChannel: [String: Int] = [:]

    /// Per-channel publisher keys (local-only). The `signingKeySeed` half is
    /// persisted in the hardware-backed Keychain
    /// (``ChannelPublisherKeyStore``, G-29) via ``persistPublisherSigningSeeds``
    /// so a relaunch keeps signing under the same key (invariant #7); the
    /// content-key `seed` re-derives from the MLS PublisherGroup on open.
    @Published public private(set) var keysByChannel: [String: ChannelPublisherKeys] = [:]

    /// Per-channel persisted publisher **signing** seed, restored from the
    /// Keychain on launch (G-29). The publisher-open path
    /// (``ChannelsComposer/freshPublisherKeys``) reuses the seed here so this
    /// device continues signing the channel under the same Ed25519 key.
    private var publisherSigningSeedByChannel: [String: Data] = [:]

    /// Per-channel **forever-monotone** publish counter, persisted at rest in
    /// the hardware-backed Keychain (``ChannelPublishCounterStore``) so it never
    /// restarts at 0 across a relaunch. The AEAD key + nonce `lakko-channels`
    /// seals each post under are HKDF-derived from
    /// `(seed, channel_id, epoch_id, sender_pubkey, message_counter)`, and the
    /// content `seed` re-derives STABLY from the persisted MLS PublisherGroup
    /// exporter — so an in-memory counter reset to 0 after a relaunch would
    /// reuse a `(key, nonce)` pair on different plaintexts (ChaCha20 keystream
    /// reuse). ``nextCounter(channelId:)`` write-throughs every bump; the
    /// counter is NEVER reset on a rotation — `epoch_id` is folded into the
    /// HKDF, so a single monotone sequence per channel is safe across epochs.
    /// (Multi-device publisher sharing of one counter is a SEPARATE pre-existing
    /// concern — the `lakko-channels` contract is per-client storage — out of
    /// scope here.)
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
        // Restore persisted publisher signing seeds so a relaunched publisher
        // keeps signing its channels under the same Ed25519 key (G-29).
        self.publisherSigningSeedByChannel = Self.loadPublisherSigningSeeds()
        // Restore persisted per-channel publish counters so a relaunched
        // publisher never restarts the AEAD counter at 0 under the stable
        // content seed + epoch (nonce-reuse defence).
        self.counterByChannel = Self.loadPublishCounters()
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
        subscriberCountByChannel[channelId] = nil
        keysByChannel[channelId] = nil
        // Deliberately KEEP this channel's publish counter (in-memory + at
        // rest): un-publishing does NOT destroy the MLS PublisherGroup content
        // seed, so a re-add of the same channel_id re-derives the same content
        // seed + epoch. The counter must stay a forever-monotone floor so a
        // re-add can never rewind it to 0 and re-collide the AEAD nonce. (This
        // is robust even though the signing seed below IS dropped — matches the
        // web client, and does not rely on the sender_pubkey changing.)
        // Drop this channel's persisted publisher signing seed (G-29) — the
        // device is no longer a publisher for it.
        publisherSigningSeedByChannel[channelId] = nil
        persistPublisherSigningSeeds()
    }

    public func updateLocalMeta(
        channelId: String,
        localName: String? = nil,
        localDescription: String? = nil,
        handle: String? = nil
    ) {
        guard let idx = channels.firstIndex(where: { $0.id == channelId })
        else { return }
        let old = channels[idx]
        channels[idx] = ChannelHandle(
            id: old.id,
            localName: localName ?? old.localName,
            localDescription: localDescription ?? old.localDescription,
            addedAtMs: old.addedAtMs,
            handle: handle ?? old.handle
        )
    }

    /// Cache the subscriber count the manage screen or the list refresh
    /// last read for `channelId`, so the channel row can show it. UI
    /// bookkeeping only: in memory, never persisted.
    public func setSubscriberCount(channelId: String, count: Int) {
        subscriberCountByChannel[channelId] = count
    }

    public func setPosts(channelId: String, posts: [PostView]) {
        postsByChannel[channelId] = posts
    }

    public func setEpoch(channelId: String, epoch: UInt32) {
        epochByChannel[channelId] = epoch
    }

    /// Install fresh publisher keys. Persists the `signingKeySeed` half to the
    /// hardware-backed Keychain (G-29) so a relaunch reuses it via
    /// ``persistedPublisherSigningSeed(channelId:)``.
    ///
    /// The publish counter is **load-else-0**: it starts at 0 only for a channel
    /// this device has never published to, and otherwise keeps its persisted
    /// forever-monotone value (mirroring the publisher-seed reuse-else-generate
    /// in ``ChannelsComposer/freshPublisherKeys``). Resetting to 0 here would
    /// reuse the AEAD `(key, nonce)` — the content seed re-derives STABLY from
    /// the MLS PublisherGroup, so the counter must never wind back.
    public func setKeys(channelId: String, keys: ChannelPublisherKeys) {
        keysByChannel[channelId] = keys
        if counterByChannel[channelId] == nil {
            counterByChannel[channelId] = 0
        }
        persistPublishCounters()
        publisherSigningSeedByChannel[channelId] = keys.signingKeySeed
        persistPublisherSigningSeeds()
    }

    /// The persisted per-channel publisher signing seed, or `nil` when this
    /// device has never published to `channelId`. The publisher-open path
    /// hands it to ``ChannelsComposer/freshPublisherKeys`` so signing
    /// continues under the same key across a relaunch (G-29).
    public func persistedPublisherSigningSeed(channelId: String) -> Data? {
        publisherSigningSeedByChannel[channelId]
    }

    /// Atomically read-and-bump the per-channel publish
    /// counter. Returns the value the NEXT post should
    /// carry; the state is updated to one past that.
    ///
    /// The new floor is **write-through**-persisted to the Keychain BEFORE the
    /// value is returned: a crash right after a post must still have advanced
    /// the persisted floor, so the same counter is never handed out twice
    /// across a relaunch (a skipped value on a crash is safe — the crate needs
    /// strict monotonicity, not gapless; a REUSED value is the catastrophic
    /// AEAD nonce reuse this defends against).
    public func nextCounter(channelId: String) -> UInt64 {
        let current = counterByChannel[channelId] ?? 0
        counterByChannel[channelId] = current + 1
        persistPublishCounters()
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
        subscriberCountByChannel = [:]
        keysByChannel = [:]
        counterByChannel = [:]
        subscriberKeysByChannel = [:]
        subscribedSeedsByChannel = [:]
        publisherSigningSeedByChannel = [:]
        persistSubscriberKeys()
        persistPublisherSigningSeeds()
        // Wipe the persisted publish counters (logout / reset). A fresh account
        // on this device starts every channel's counter at 0 again — safe
        // because a fresh account also mints fresh publisher keys + seeds.
        persistPublishCounters()
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

    // MARK: - Publisher signing-seed persistence (G-29)

    /// One row in the persisted Keychain blob.
    private struct PersistedPublisherKey: Codable {
        let channelId: String
        /// Hex-encoded 32-byte Ed25519 signing seed.
        let signingSeedHex: String
    }

    private func persistPublisherSigningSeeds() {
        // G-29 — the Ed25519 signing seed is hardware-backed-Keychain material,
        // not `UserDefaults` plaintext (invariant #7). An empty set wipes the
        // item (remove-channel / reset / logout).
        if publisherSigningSeedByChannel.isEmpty {
            ChannelPublisherKeyStore.delete()
            return
        }
        let rows: [PersistedPublisherKey] =
            publisherSigningSeedByChannel.map { (channelId, seed) in
                PersistedPublisherKey(
                    channelId: channelId,
                    signingSeedHex: ChannelsComposer.bytesToHex(seed)
                )
            }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        ChannelPublisherKeyStore.save(data)
    }

    private static func loadPublisherSigningSeeds() -> [String: Data] {
        guard
            let data = ChannelPublisherKeyStore.load(),
            let rows = try? JSONDecoder().decode(
                [PersistedPublisherKey].self,
                from: data
            )
        else {
            return [:]
        }
        var out: [String: Data] = [:]
        for row in rows {
            guard let seed = Data(hexString: row.signingSeedHex) else { continue }
            out[row.channelId] = seed
        }
        return out
    }

    // MARK: - Publish-counter persistence (AEAD nonce-reuse defence)

    /// One row in the persisted Keychain blob.
    private struct PersistedPublishCounter: Codable {
        let channelId: String
        /// Forever-monotone per-channel publish counter.
        let counter: UInt64
    }

    private func persistPublishCounters() {
        // The counter is the AEAD nonce input `lakko-channels` requires to be
        // strictly monotone within `(channel_id, epoch_id, sender_pubkey)`
        // BETWEEN SESSIONS; the content seed re-derives stably from the MLS
        // PublisherGroup, so a relaunch that restarts it at 0 reuses a
        // `(key, nonce)` pair (ChaCha20 keystream reuse). Kept in the same
        // hardware-backed Keychain as the publisher signing seed for a single
        // consistent store (the counter is not itself secret — it rides in the
        // clear post header on the wire). An empty set wipes the item
        // (remove-all / reset / logout).
        if counterByChannel.isEmpty {
            ChannelPublishCounterStore.delete()
            return
        }
        let rows: [PersistedPublishCounter] =
            counterByChannel.map { (channelId, counter) in
                PersistedPublishCounter(channelId: channelId, counter: counter)
            }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        ChannelPublishCounterStore.save(data)
    }

    private static func loadPublishCounters() -> [String: UInt64] {
        guard
            let data = ChannelPublishCounterStore.load(),
            let rows = try? JSONDecoder().decode(
                [PersistedPublishCounter].self,
                from: data
            )
        else {
            return [:]
        }
        var out: [String: UInt64] = [:]
        for row in rows {
            out[row.channelId] = row.counter
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
    /// The `@handle` this device knows for the channel (claimed on create,
    /// or the one it was found by), without the leading `@`. Empty when
    /// unknown. Local-only, like the name.
    public let handle: String

    public init(
        id: String,
        localName: String,
        localDescription: String,
        addedAtMs: UInt64,
        handle: String = ""
    ) {
        self.id = id
        self.localName = localName
        self.localDescription = localDescription
        self.addedAtMs = addedAtMs
        self.handle = handle
    }
}
