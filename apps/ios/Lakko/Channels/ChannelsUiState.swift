// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
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
///
/// ## What survives a relaunch
///
/// Once ``bind(accountIdB64:)`` has run (the root screen does it at
/// construction): the channel list and the subscriber-side recovered seeds
/// (``ChannelLogStore``, one sealed file per account), the publisher keys
/// (content seed + signing seed, ``ChannelPublisherKeyStore``), the publish
/// counters (``ChannelPublishCounterStore``) and the subscriber keypairs
/// (``ChannelSubscriberKeyStore``) — each of the three Keychain blobs one item
/// per account since 2026-09-30, so a second account signed in on the same
/// phone never inherits the first one's channel keys (a reused subscriber key
/// would link the two accounts at the channels server). Before a bind every
/// map is empty and nothing is written. Posts, epochs and subscriber counts
/// are server caches and are re-read.
@MainActor
public final class ChannelsUiState: ObservableObject {

    /// User's local channel list, newest-first. Persisted per account through
    /// ``ChannelLogStore`` on every mutation once bound.
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

    /// Per-channel publisher keys (local-only). Both halves are persisted in
    /// the hardware-backed Keychain (``ChannelPublisherKeyStore``, G-29) via
    /// ``persistPublisherKeys`` and restored by ``bind(accountIdB64:)``, so a
    /// relaunched publisher posts and reads under the same content seed its
    /// subscribers hold and keeps signing under the same Ed25519 key
    /// (invariant #7).
    ///
    /// The content seed is persisted here as well as re-derivable from the
    /// PublisherGroup, which lives in the durable channel provider since G-50
    /// (``ChannelPublisherIdentity``; before, every channel path built a
    /// throwaway provider and a relaunch founded a fresh group). A commit's
    /// steps write the N+1 seed back here (``ChannelPublisherCommits``).
    @Published public private(set) var keysByChannel: [String: ChannelPublisherKeys] = [:]

    /// Per-channel persisted publisher **signing** seed, restored from the
    /// Keychain on launch (G-29). The publisher-open path
    /// (``ChannelsComposer/freshPublisherKeys``) reuses the seed here so this
    /// device continues signing the channel under the same Ed25519 key.
    private var publisherSigningSeedByChannel: [String: Data] = [:]

    /// Per-channel persisted publisher **content** seed, stored in the same
    /// Keychain blob as the signing seed. ``bind(accountIdB64:)`` rebuilds
    /// ``keysByChannel`` from the two maps for every channel in the list that
    /// has both.
    private var publisherContentSeedByChannel: [String: Data] = [:]

    /// The per-account sealed store behind ``channels`` and
    /// ``subscribedSeedsByChannel``. Written through on every mutation once
    /// ``boundAccountIdB64`` is set; unbound mutations stay in memory.
    private let logStore = ChannelLogStore()
    private var boundAccountIdB64: String?

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

    /// The account's stored publish counters exist but could not be read or
    /// decoded at ``bind(accountIdB64:)``. While set, no counter is handed
    /// out and the stored item is never overwritten: the signing seeds and
    /// content seeds are a separate Keychain item that may have read back
    /// fine, and a counter restarted at 0 under them would reuse an AEAD
    /// `(key, nonce)` (G-51). ``nextCounter(channelId:)`` reads again first.
    private var publishCountersUnreadable = false

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
    /// that epoch. Persisted per account through ``ChannelLogStore``
    /// (AES-GCM under a Keychain wrap key) so a relaunched subscriber
    /// reads without waiting for a fresh fan-out; never sent off-device.
    /// UI shows a "waiting for seed delivery" state until the map has an
    /// entry for the channel.
    @Published public private(set)
    var subscribedSeedsByChannel: [String: [UInt32: Data]] = [:]

    /// ADR-0087 — the per-channel facts metadata v3 and the newer routes add
    /// (``ChannelExtras``): the public flag, the photo, the allowed
    /// reactions, the ban list, the read key of a public channel.
    @Published public private(set) var extrasByChannel: [String: ChannelExtras] = [:]

    /// **Legacy** `UserDefaults` key — the pre-G-29 plaintext, device-wide
    /// storage location of the subscriber keypairs. Never read; removed by
    /// ``forget(accountIdB64:)``.
    nonisolated public static let subscriberKeysStorageKey =
        "lakko.channels.subscriber-keys.v1"

    /// Unbound: every map empty. ``bind(accountIdB64:)`` loads one account's.
    public init() {}

    /// Construct already bound to `accountIdB64` — the shape the root
    /// screen's `@StateObject` uses so the list is loaded before the first
    /// body evaluation and no follow-up task re-renders the tab.
    public convenience init(accountIdB64: String) {
        self.init()
        bind(accountIdB64: accountIdB64)
    }

    // MARK: - Binding

    /// Attach the state to one account: load its persisted channel list and
    /// subscribed seeds, and rebuild the publisher keys of every listed
    /// channel whose content + signing seeds the Keychain holds. From here on
    /// every mutation of the list or the seed map writes through.
    ///
    /// Rebinding to a different account replaces the in-memory list and the
    /// three Keychain-backed maps with that account's; the previous account's
    /// stores are left as they are. The publish counters are loaded, never
    /// reset — each account's is a forever-monotone floor of its own.
    public func bind(accountIdB64: String) {
        boundAccountIdB64 = accountIdB64
        // Restore the account's subscriber keypairs so a fresh launch can poll
        // for envelopes without the user re-subscribing manually.
        subscriberKeysByChannel = Self.loadSubscriberKeys(accountIdB64: accountIdB64)
        // Restore the account's publisher seeds (signing + content) so a
        // relaunched publisher keeps signing its channels under the same
        // Ed25519 key (G-29) and can rebuild its keys below.
        let publisherSeeds = Self.loadPublisherKeys(accountIdB64: accountIdB64)
        publisherSigningSeedByChannel = publisherSeeds.signing
        publisherContentSeedByChannel = publisherSeeds.content
        // Restore the account's per-channel publish counters so a relaunched
        // publisher never restarts the AEAD counter at 0 under the stable
        // content seed + epoch (nonce-reuse defence).
        if let counters = Self.publishCounters(
            from: ChannelPublishCounterStore.read(accountIdB64: accountIdB64)) {
            counterByChannel = counters
            publishCountersUnreadable = false
        } else {
            NSLog("Channels: the stored publish counters could not be read; publishing waits until they can")
            counterByChannel = [:]
            publishCountersUnreadable = true
        }
        if let payload = logStore.get(accountIdB64: accountIdB64) {
            channels = payload.channelHandles
            subscribedSeedsByChannel = payload.subscribedSeedsByChannel
        } else {
            channels = []
            subscribedSeedsByChannel = [:]
        }
        keysByChannel = ChannelPublisherReopen.keys(
            channels: channels,
            contentSeeds: publisherContentSeedByChannel,
            signingSeeds: publisherSigningSeedByChannel
        )
        ownerKeyGenerationByChannel = Self.loadOwnerGenerations(accountIdB64: accountIdB64)
        extrasByChannel = ChannelExtrasStore.load(accountIdB64)
        publishChannelPhotos()
    }

    // MARK: - ADR-0087 channel extras

    /// The extras held for `channelId` (empty when none).
    public func extras(_ channelId: String) -> ChannelExtras {
        extrasByChannel[channelId] ?? ChannelExtras()
    }

    /// Change the extras of `channelId` and write them through.
    public func updateExtras(_ channelId: String, _ change: (inout ChannelExtras) -> Void) {
        var next = extras(channelId)
        change(&next)
        guard next != extrasByChannel[channelId] else { return }
        extrasByChannel[channelId] = next
        persistExtras()
        publishChannelPhotos()
    }

    /// Section 13a — keep a ban list when it is strictly newer than the one
    /// held. Returns `true` when it was kept.
    @discardableResult
    public func recordBanList(channelId: String, updatedAtMs: UInt64, authors: [Data]) -> Bool {
        let held = extras(channelId)
        guard held.banUpdatedAtMs == 0
            || banListIsNewer(incomingUpdatedAtMs: updatedAtMs, storedUpdatedAtMs: held.banUpdatedAtMs)
        else { return false }
        updateExtras(channelId) {
            $0.banUpdatedAtMs = updatedAtMs
            $0.bannedAuthors = authors
        }
        return true
    }

    private func persistExtras() {
        guard let account = boundAccountIdB64 else { return }
        ChannelExtrasStore.save(extrasByChannel, account)
    }

    private func publishChannelPhotos() {
        var photos: [String: ProfileAvatar] = [:]
        for (id, extras) in extrasByChannel {
            if let avatar = extras.avatar { photos[id] = avatar }
        }
        ChannelPhotoDirectory.shared.replace(photos)
    }

    // MARK: - ADR-0073 phase 2 — channels the old keys owned (UX 16.6)

    /// The account's key generation the session reports (ADR-0073; `1` =
    /// never reset). Set by the shell at sign-in; `nil` means unknown and
    /// nothing is claimed.
    @Published public var currentKeyGeneration: UInt64?

    /// Per channel this device created: the key generation the owner key was
    /// registered under. A channel's publishing key derives from the account
    /// seed, so a later generation means the registered key is gone with the
    /// old seed. Persisted per account in `UserDefaults` (non-secret).
    @Published public private(set) var ownerKeyGenerationByChannel: [String: UInt64] = [:]

    nonisolated private static func ownerGenerationKey(_ accountIdB64: String) -> String {
        "lakko.channels.owner-generation.\(accountIdB64)"
    }

    private static func loadOwnerGenerations(accountIdB64: String) -> [String: UInt64] {
        (UserDefaults.standard.dictionary(forKey: ownerGenerationKey(accountIdB64)) as? [String: NSNumber])?
            .mapValues { $0.uint64Value } ?? [:]
    }

    private func persistOwnerGenerations() {
        guard let account = boundAccountIdB64,
              let gate = AccountStoreGate.enter(account) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(
            ownerKeyGenerationByChannel.mapValues { NSNumber(value: $0) },
            forKey: Self.ownerGenerationKey(account))
    }

    /// ADR-0074 section 3.5 — the account's keys were rotated and the owner
    /// key of the channels this device created was handed to the new seed's
    /// key (``OwnedChannelGenerations``): re-read the recorded generations
    /// and take the session's new one, in that order, so no channel is read
    /// as orphaned in between.
    public func keysRotated(currentKeyGeneration generation: UInt64?) {
        if let account = boundAccountIdB64 {
            ownerKeyGenerationByChannel = Self.loadOwnerGenerations(accountIdB64: account)
        }
        if let generation { currentKeyGeneration = generation }
    }

    /// Record, at creation, which key generation registered the owner key.
    public func recordOwnerKeyGeneration(channelId: String, generation: UInt64) {
        ownerKeyGenerationByChannel[channelId] = generation
        persistOwnerGenerations()
    }

    /// UX 16.6 — whether `channelId` is a channel this device owned under keys
    /// a reset has since replaced. Only the owning device can know; a channel
    /// created before the generation was recorded reads as current.
    public func orphanedByReset(channelId: String) -> Bool {
        ChannelKeyReset.orphaned(
            createdAtGeneration: ownerKeyGenerationByChannel[channelId],
            currentGeneration: currentKeyGeneration)
    }

    /// UX 16.6 — `Remove from this device`: drop the channel and its publisher
    /// state (the seeds can sign nothing the server accepts any more).
    public func removeOrphanedChannel(_ channelId: String) {
        removeChannel(channelId)
        publisherSigningSeedByChannel[channelId] = nil
        publisherContentSeedByChannel[channelId] = nil
        persistPublisherKeys()
        ownerKeyGenerationByChannel[channelId] = nil
        persistOwnerGenerations()
    }

    // MARK: - Mutations

    /// ADR-0087 section 9 — this device unsubscribed, or subscribed to a
    /// channel. Wired by the signed-in shell, which reaches the account's
    /// other devices.
    public var onUnsubscribedHere: ((String) -> Void)?
    public var onSubscribedHere: ((String) -> Void)?
    /// UX 9.5 — a publisher removed this device from a private channel; the
    /// name is the notice's, `nil` when the open channel showed a dialog.
    public var onRemovedHere: ((_ channelId: String, _ name: String?) -> Void)?
    /// A newer shared record (name, description) was accepted for a channel:
    /// the owner's device republishes the channel's public record
    /// (`group-and-channel-privacy.md` 4.5.4).
    public var onSharedMetaAccepted: ((_ channelId: String, _ meta: SharedChannelMeta) -> Void)?

    public func addChannel(_ handle: ChannelHandle) {
        // Id-idempotent: re-adding refreshes the local
        // metadata rather than appending a duplicate.
        var without = channels.filter { $0.id != handle.id }
        without.insert(handle, at: 0)
        channels = without
        persistChannelLog()
    }

    /// ADR-0087 section 15 — this device was removed as a publisher (or
    /// stepped down) but the account still reads the channel as a
    /// subscriber: the publisher keys go, the channel and its posts stay.
    /// The publish counter stays a floor, as in ``removeChannel(_:)``.
    public func dropPublisherKeys(_ channelId: String) {
        keysByChannel[channelId] = nil
        publisherSigningSeedByChannel[channelId] = nil
        publisherContentSeedByChannel[channelId] = nil
        persistPublisherKeys()
    }

    /// ADR-0087 phase B — the channel was deleted (`410 channel-deleted`,
    /// or this device's owner deleted it): the section 9 end of membership,
    /// here and, through ``onRemovedHere``, on the account's other devices.
    public func endDeleted(_ channelId: String) {
        // A publisher request for a channel that is gone waits for nothing.
        if let account = AcceptedContactsStore.boundAccountIdB64 {
            PendingPublisherRequests.drop(channelIdHex: channelId, accountIdB64: account)
        }
        guard channels.contains(where: { $0.id == channelId }) else { return }
        let name = channels.first { $0.id == channelId }?.title
        ChannelMemberCards.forgetUploads(channelId: channelId)
        clearSubscriberKeys(channelId)
        removeChannel(channelId)
        onRemovedHere?(channelId, name)
    }

    public func removeChannel(_ channelId: String) {
        channels.removeAll { $0.id == channelId }
        postsByChannel[channelId] = nil
        epochByChannel[channelId] = nil
        subscriberCountByChannel[channelId] = nil
        keysByChannel[channelId] = nil
        persistChannelLog()
        if extrasByChannel.removeValue(forKey: channelId) != nil {
            persistExtras()
            publishChannelPhotos()
        }
        // Deliberately KEEP this channel's publish counter (in-memory + at
        // rest): un-publishing does NOT destroy the MLS PublisherGroup content
        // seed, so a re-add of the same channel_id re-derives the same content
        // seed + epoch. The counter must stay a forever-monotone floor so a
        // re-add can never rewind it to 0 and re-collide the AEAD nonce. (This
        // is robust even though the signing seed below IS dropped — matches the
        // web client, and does not rely on the sender_pubkey changing.)
        // Drop this channel's persisted publisher seeds (G-29) — the device
        // is no longer a publisher for it.
        publisherSigningSeedByChannel[channelId] = nil
        publisherContentSeedByChannel[channelId] = nil
        persistPublisherKeys()
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
            handle: handle ?? old.handle,
            sharedName: old.sharedName,
            sharedDescription: old.sharedDescription,
            sharedUpdatedAtMs: old.sharedUpdatedAtMs,
            sharedTopic: old.sharedTopic,
            sharedLanguage: old.sharedLanguage,
            sharedListed: old.sharedListed
        )
        persistChannelLog()
    }

    /// ADR-0051 §4 — keep `meta` as the channel's shared name and description
    /// if it is strictly newer than what the row holds (the core's
    /// `channelMetaIsNewer`; equal is not newer, so a rotation re-issue of the
    /// current record changes nothing). Persists through the channel log.
    /// Returns `true` when the row changed. A channel not in the list is not
    /// created here (a reader lists it when subscribing).
    @discardableResult
    public func recordSharedMeta(channelId: String, meta: SharedChannelMeta) -> Bool {
        guard let idx = channels.firstIndex(where: { $0.id == channelId }) else { return false }
        let old = channels[idx]
        if old.sharedUpdatedAtMs > 0,
           !channelMetaIsNewer(
               incomingUpdatedAtMs: meta.updatedAtMs, storedUpdatedAtMs: old.sharedUpdatedAtMs) {
            return false
        }
        channels[idx] = ChannelHandle(
            id: old.id,
            localName: old.localName,
            localDescription: old.localDescription,
            addedAtMs: old.addedAtMs,
            handle: old.handle,
            sharedName: meta.name,
            sharedDescription: meta.description,
            sharedUpdatedAtMs: meta.updatedAtMs,
            sharedTopic: meta.topic,
            sharedLanguage: meta.language,
            sharedListed: meta.listed
        )
        persistChannelLog()
        updateExtras(channelId) {
            $0.handle = meta.handle.map(StoredConversationHandle.init)
            $0.recordPublic = meta.listed
            $0.avatar = ProfileAvatar(meta.avatar)
            $0.reactions = meta.reactions.map(StoredAllowedReactions.init)
        }
        onSharedMetaAccepted?(channelId, meta)
        return true
    }

    /// The newest shared record held for `channelId`, or `nil`.
    public func sharedMeta(channelId: String) -> SharedChannelMeta? {
        guard let base = channels.first(where: { $0.id == channelId })?.sharedMeta else { return nil }
        let extras = extras(channelId)
        return SharedChannelMeta(
            name: base.name, description: base.description, updatedAtMs: base.updatedAtMs,
            topic: base.topic, language: base.language, listed: base.listed,
            handle: extras.handle?.ffi, avatar: extras.avatar?.ffi, reactions: extras.reactions?.ffi)
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

    /// Install fresh publisher keys. Persists both halves to the
    /// hardware-backed Keychain (G-29): the `signingKeySeed` so a re-open
    /// reuses it via ``persistedPublisherSigningSeed(channelId:)``, and the
    /// content `seed` so ``bind(accountIdB64:)`` rebuilds the keys after a
    /// relaunch.
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
        // Best effort here: nothing is published yet, and every post persists
        // its own floor before it goes out (``nextCounter(channelId:)``).
        try? persistPublishCounters()
        publisherSigningSeedByChannel[channelId] = keys.signingKeySeed
        publisherContentSeedByChannel[channelId] = keys.seed
        persistPublisherKeys()
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
    ///
    /// Throws, and hands out no counter, when the new floor did not reach the
    /// Keychain: no account is bound (``StaleBindingError``), the account's
    /// session has ended (``AccountStoreGate/Sealed``), or the Keychain
    /// refused the write. The caller publishes nothing then; the value is
    /// skipped, never reused.
    ///
    /// Also throws ``ChannelPublishCounterStoreError/floorUnreadable`` while
    /// the stored counters cannot be read: a channel absent from a map that
    /// failed to load is not a channel that never published (G-51).
    public func nextCounter(channelId: String) throws -> UInt64 {
        guard let account = boundAccountIdB64 else { throw StaleBindingError() }
        if publishCountersUnreadable {
            try rereadPublishCounters(accountIdB64: account)
        }
        let current = counterByChannel[channelId] ?? 0
        counterByChannel[channelId] = current + 1
        try persistPublishCounters()
        return current
    }

    /// Read the stored counters again after ``bind(accountIdB64:)`` could
    /// not, and keep the higher of each stored and in-memory value. Throws
    /// ``ChannelPublishCounterStoreError/floorUnreadable`` while they still
    /// cannot be read.
    private func rereadPublishCounters(accountIdB64: String) throws {
        guard let stored = Self.publishCounters(
            from: ChannelPublishCounterStore.read(accountIdB64: accountIdB64)) else {
            throw ChannelPublishCounterStoreError.floorUnreadable
        }
        counterByChannel = Self.higherFloors(counterByChannel, stored)
        publishCountersUnreadable = false
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
        persistChannelLog()
    }

    /// Cache the recovered content-key seed for one epoch
    /// after the subscriber successfully opened that epoch's
    /// seed envelope (ADR-0031). Merges into the channel's
    /// `epoch -> seed` map so multiple retained epochs coexist.
    public func setSubscribedSeed(channelId: String, epoch: UInt32, seed: Data) {
        var byEpoch = subscribedSeedsByChannel[channelId] ?? [:]
        byEpoch[epoch] = seed
        subscribedSeedsByChannel[channelId] = byEpoch
        persistChannelLog()
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
        publisherContentSeedByChannel = [:]
        // Every signing seed goes with the counters, so no floor is needed.
        publishCountersUnreadable = false
        persistSubscriberKeys()
        persistPublisherKeys()
        // Wipe the persisted publish counters (logout / reset). A fresh account
        // on this device starts every channel's counter at 0 again — safe
        // because a fresh account also mints fresh publisher keys + seeds.
        try? persistPublishCounters()
        if let account = boundAccountIdB64 {
            logStore.delete(accountIdB64: account)
            ChannelExtrasStore.delete(account)
        }
        extrasByChannel = [:]
        publishChannelPhotos()
    }

    /// Forget-device: delete everything this device keeps for one account's
    /// channels — the sealed channel log, the three Keychain blobs (publisher
    /// seeds, publish counters, subscriber keypairs) and the rotation state —
    /// plus the old device-wide items nothing reads any more. Other accounts'
    /// channels are untouched. Dropping the publish counters with the signing
    /// seeds is safe: the next publisher open of this account mints a fresh
    /// signing key, so the counter restarts under a different sender key.
    nonisolated public static func forget(accountIdB64: String) {
        ChannelLogStore().delete(accountIdB64: accountIdB64)
        ChannelPublisherKeyStore.delete(accountIdB64: accountIdB64)
        ChannelPublishCounterStore.delete(accountIdB64: accountIdB64)
        ChannelSubscriberKeyStore.delete(accountIdB64: accountIdB64)
        ChannelRotationStateStore.clear(accountIdB64: accountIdB64)
        ChannelExtrasStore.delete(accountIdB64)
        ChannelLinkStore.delete(accountIdB64)
        ChannelAdmissionKeyStore.delete(accountIdB64: accountIdB64)
        ChannelAdmissionSeedShare.forget(accountIdB64: accountIdB64)
        ChannelInviteCardStore.shared.delete(accountIdB64: accountIdB64)
        PendingPublisherRequests.clear(accountIdB64: accountIdB64)
        PendingChannelReactionStore.delete(accountIdB64)
        ChannelPublisherKeyStore.deleteLegacy()
        ChannelPublishCounterStore.deleteLegacy()
        ChannelSubscriberKeyStore.deleteLegacy()
        UserDefaults.standard.removeObject(forKey: subscriberKeysStorageKey)
        UserDefaults.standard.removeObject(forKey: ownerGenerationKey(accountIdB64))
    }

    // MARK: - Channel-log persistence (list + subscribed seeds)

    /// Write the channel list and the subscribed seeds through to the bound
    /// account's sealed file. A no-op before ``bind(accountIdB64:)``.
    private func persistChannelLog() {
        guard let account = boundAccountIdB64 else { return }
        logStore.put(
            accountIdB64: account,
            payload: ChannelLogPayload(
                channels: channels,
                subscribedSeedsByChannel: subscribedSeedsByChannel
            )
        )
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
        // item (un-follow-all / reset). Unbound: nothing is written.
        guard let account = boundAccountIdB64 else { return }
        if subscriberKeysByChannel.isEmpty {
            ChannelSubscriberKeyStore.delete(accountIdB64: account)
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
        ChannelSubscriberKeyStore.save(data, accountIdB64: account)
    }

    private static func loadSubscriberKeys(accountIdB64: String) -> [String: ChannelSubscriberKeys] {
        guard
            let data = ChannelSubscriberKeyStore.load(accountIdB64: accountIdB64),
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
            // ADR-0060 section 3 — a subscriber key is Ed25519 now (it signs
            // the member card). A row written before that holds an X25519
            // pair, whose secret re-read as an Ed25519 seed names a different
            // public key: drop it, so the channel is re-subscribed with a
            // fresh Ed25519 key rather than read under one the server refuses.
            guard let rebuilt = try? ChannelSubscriberKeypair.fromSecret(secret: secret),
                  rebuilt.`public`() == pub else { continue }
            out[row.channelId] = ChannelSubscriberKeys(
                secret: secret,
                publicKey: pub
            )
        }
        return out
    }

    // MARK: - Publisher seed persistence (G-29)

    /// One row in the persisted Keychain blob.
    private struct PersistedPublisherKey: Codable {
        let channelId: String
        /// Hex-encoded 32-byte Ed25519 signing seed.
        let signingSeedHex: String
        /// Hex-encoded 32-byte channel content seed. Absent on a row written
        /// before the content seed was persisted; such a channel keeps its
        /// signing seed but cannot be re-opened as a publisher on bind.
        let seedHex: String?
    }

    private func persistPublisherKeys() {
        // G-29 — both seeds are hardware-backed-Keychain material, not
        // `UserDefaults` plaintext (invariant #7). An empty set wipes the item
        // (remove-channel / reset). Unbound: nothing is written.
        guard let account = boundAccountIdB64 else { return }
        if publisherSigningSeedByChannel.isEmpty {
            ChannelPublisherKeyStore.delete(accountIdB64: account)
            return
        }
        let rows: [PersistedPublisherKey] =
            publisherSigningSeedByChannel.map { (channelId, signingSeed) in
                PersistedPublisherKey(
                    channelId: channelId,
                    signingSeedHex: ChannelsComposer.bytesToHex(signingSeed),
                    seedHex: publisherContentSeedByChannel[channelId].map(ChannelsComposer.bytesToHex)
                )
            }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        ChannelPublisherKeyStore.save(data, accountIdB64: account)
    }

    private static func loadPublisherKeys(
        accountIdB64: String
    ) -> (signing: [String: Data], content: [String: Data]) {
        guard
            let data = ChannelPublisherKeyStore.load(accountIdB64: accountIdB64),
            let rows = try? JSONDecoder().decode(
                [PersistedPublisherKey].self,
                from: data
            )
        else {
            return ([:], [:])
        }
        var signing: [String: Data] = [:]
        var content: [String: Data] = [:]
        for row in rows {
            guard let signingSeed = Data(hexString: row.signingSeedHex) else { continue }
            signing[row.channelId] = signingSeed
            if let hex = row.seedHex, let seed = Data(hexString: hex) {
                content[row.channelId] = seed
            }
        }
        return (signing, content)
    }

    // MARK: - Publish-counter persistence (AEAD nonce-reuse defence)

    /// One row in the persisted Keychain blob.
    private struct PersistedPublishCounter: Codable {
        let channelId: String
        /// Forever-monotone per-channel publish counter.
        let counter: UInt64
    }

    private func persistPublishCounters() throws {
        // The counter is the AEAD nonce input `lakko-channels` requires to be
        // strictly monotone within `(channel_id, epoch_id, sender_pubkey)`
        // BETWEEN SESSIONS; the content seed re-derives stably from the MLS
        // PublisherGroup, so a relaunch that restarts it at 0 reuses a
        // `(key, nonce)` pair (ChaCha20 keystream reuse). Kept in the same
        // hardware-backed Keychain as the publisher signing seed for a single
        // consistent store (the counter is not itself secret — it rides in the
        // clear post header on the wire). An empty set wipes the item
        // (remove-all / reset). Unbound: nothing is written.
        guard let account = boundAccountIdB64 else { return }
        // Never written over a stored item that could not be read: it may
        // hold floors this map does not know.
        if publishCountersUnreadable { throw ChannelPublishCounterStoreError.floorUnreadable }
        if counterByChannel.isEmpty {
            ChannelPublishCounterStore.delete(accountIdB64: account)
            return
        }
        let rows: [PersistedPublishCounter] =
            counterByChannel.map { (channelId, counter) in
                PersistedPublishCounter(channelId: channelId, counter: counter)
            }
        let data = try JSONEncoder().encode(rows)
        try ChannelPublishCounterStore.save(data, accountIdB64: account)
    }

    /// The counters a read found: empty for no item, `nil` for an item that
    /// could not be read or decoded (never read as "no floor", G-51).
    nonisolated static func publishCounters(
        from read: ChannelPublishCounterStore.Read
    ) -> [String: UInt64]? {
        let data: Data
        switch read {
        case .absent: return [:]
        case .unreadable: return nil
        case .present(let bytes): data = bytes
        }
        guard let rows = try? JSONDecoder().decode([PersistedPublishCounter].self, from: data) else {
            return nil
        }
        var out: [String: UInt64] = [:]
        for row in rows {
            out[row.channelId] = max(out[row.channelId] ?? 0, row.counter)
        }
        return out
    }

    /// Per channel, the higher of the two floors.
    nonisolated static func higherFloors(
        _ a: [String: UInt64], _ b: [String: UInt64]
    ) -> [String: UInt64] {
        a.merging(b) { max($0, $1) }
    }
}

/// The pure decision ``ChannelsUiState/bind(accountIdB64:)`` takes: which
/// listed channels come back as publisher channels after a relaunch.
///
/// A channel is re-opened as a publisher only when it is in the persisted
/// list AND the Keychain holds both its content seed and its signing seed,
/// each 32 bytes. A seed left behind for a channel that is no longer listed
/// is not enough on its own (the row is what the user sees), and a listed
/// channel with only a signing seed (a row written before the content seed
/// was persisted) reads as a subscriber channel rather than a publisher
/// channel with a guessed key. Free of the state object so it is testable
/// without one.
public enum ChannelPublisherReopen {
    public static func keys(
        channels: [ChannelHandle],
        contentSeeds: [String: Data],
        signingSeeds: [String: Data]
    ) -> [String: ChannelPublisherKeys] {
        var out: [String: ChannelPublisherKeys] = [:]
        for channel in channels {
            guard let seed = contentSeeds[channel.id], seed.count == 32,
                  let signing = signingSeeds[channel.id], signing.count == 32
            else { continue }
            out[channel.id] = ChannelPublisherKeys(seed: seed, signingKeySeed: signing)
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
    /// ADR-0051 §4 — the name from the newest shared metadata record this
    /// device has read (its own, for a publisher). Empty until one lands;
    /// wins over `localName` for the title.
    public let sharedName: String
    /// ADR-0051 §4 — the description from the same record; shown only when
    /// present.
    public let sharedDescription: String
    /// ADR-0051 §4 — the record's stamp; `0` when none was read. A newer
    /// record replaces the fields, an older or equal one is ignored.
    public let sharedUpdatedAtMs: UInt64
    /// ADR-0059 metadata v2 — the record's topic id (0 = Other), language
    /// (empty = not set) and whether the owner listed the channel in Discover.
    /// What lets a member share only listed channels in a group (section 6).
    public let sharedTopic: UInt8
    public let sharedLanguage: String
    public let sharedListed: Bool

    public init(
        id: String,
        localName: String,
        localDescription: String,
        addedAtMs: UInt64,
        handle: String = "",
        sharedName: String = "",
        sharedDescription: String = "",
        sharedUpdatedAtMs: UInt64 = 0,
        sharedTopic: UInt8 = 0,
        sharedLanguage: String = "",
        sharedListed: Bool = false
    ) {
        self.id = id
        self.localName = localName
        self.localDescription = localDescription
        self.addedAtMs = addedAtMs
        self.handle = handle
        self.sharedName = sharedName
        self.sharedDescription = sharedDescription
        self.sharedUpdatedAtMs = sharedUpdatedAtMs
        self.sharedTopic = sharedTopic
        self.sharedLanguage = sharedLanguage
        self.sharedListed = sharedListed
    }

    /// The newest shared record this row holds, or `nil` when none landed.
    public var sharedMeta: SharedChannelMeta? {
        guard sharedUpdatedAtMs > 0, !sharedName.isEmpty else { return nil }
        return SharedChannelMeta(
            name: sharedName, description: sharedDescription, updatedAtMs: sharedUpdatedAtMs,
            topic: sharedTopic, language: sharedLanguage, listed: sharedListed)
    }

    /// ADR-0051 §5 — the title: the shared name, else the local name, else the
    /// bare handle, else "Channel". Mirrors web `channelTitle`.
    public var title: String {
        let shared = sharedName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !shared.isEmpty { return shared }
        let local = localName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !local.isEmpty { return local }
        let bare = handle.trimmingCharacters(in: .whitespacesAndNewlines).drop(while: { $0 == "@" })
        return bare.isEmpty ? Localization.text("channels.fallback_name") : String(bare)
    }

    /// The description to show: the shared one when a record landed, else
    /// the local one (a publisher before its first record).
    public var displayDescription: String {
        sharedMeta != nil ? sharedDescription : localDescription
    }
}

/// ADR-0073 phase 2 (UX 16.6) — the one rule: a channel created under an
/// earlier key generation than the session's current one was orphaned by a
/// reset. Unknown on either side claims nothing (the client "does not
/// invent a signal").
public enum ChannelKeyReset {
    public static func orphaned(createdAtGeneration: UInt64?, currentGeneration: UInt64?) -> Bool {
        guard let createdAtGeneration, let currentGeneration else { return false }
        return currentGeneration > createdAtGeneration
    }
}
