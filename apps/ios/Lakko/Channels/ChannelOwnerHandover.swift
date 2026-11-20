// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0074 section 3.5, "Channels" — after a rotation of the account keys the
// owner key of every channel this account owns moves to the key the NEW seed
// derives: `PUT /v1/channels/{id}/owner`, body = the new owner key, the owner
// proof by the OLD seed's key.
//
// The handover is tried by the rotating device and, idempotently, by every
// kept device once it holds the new keys: a device knows only the channels
// it created, and a handover naming the key that already owns the channel is
// a `204` that changes nothing.
//
// A co-publisher key this account holds in someone else's channel derives
// from the old seed too and can only be replaced by that channel's owner; the
// client asks nothing of them (a stated residual of the ADR).

enum ChannelOwnerHandover {

    /// What one channel's handover came to.
    enum Outcome: Equatable {
        /// The owner key is the new seed's (it was replaced now, or before).
        case owned
        /// Neither the old nor the new key owns the channel: this account is
        /// not its owner (a co-publisher). Nothing to do.
        case notOwner
        /// No verdict (offline, a limit, `5xx`): owed.
        case retry
    }

    /// What follows the attempt signed by the OLD key.
    enum AfterOldKey: Equatable {
        case owned
        /// `401 owner-proof-required` or `409 owner-changed`: the old key no
        /// longer owns the channel. Ask with the NEW key whether the handover
        /// already landed.
        case probeWithNewKey
        case retry
    }

    static func afterOldKey(_ error: Error?) -> AfterOldKey {
        guard let error else { return .owned }
        guard let api = error as? ChannelsApiError else { return .retry }
        switch api.status {
        case 401, 409: return .probeWithNewKey
        default: return .retry
        }
    }

    /// What follows the probe signed by the NEW key (naming itself).
    static func afterNewKey(_ error: Error?) -> Outcome {
        guard let error else { return .owned }
        guard let api = error as? ChannelsApiError else { return .retry }
        switch api.status {
        case 401: return .notOwner
        default: return .retry
        }
    }

    /// Hand one channel over. `put(signing, naming)` is
    /// ``ChannelsClient/replaceOwner(channelIdHex:signingIdentity:newOwnerIdentity:)``.
    static func handOver(
        old: Identity,
        new: Identity,
        put: (_ signing: Identity, _ naming: Identity) async throws -> Void
    ) async -> Outcome {
        var failure: Error?
        do { try await put(old, new) } catch { failure = error }
        switch afterOldKey(failure) {
        case .owned: return .owned
        case .retry: return .retry
        case .probeWithNewKey:
            var probeFailure: Error?
            do { try await put(new, new) } catch { probeFailure = error }
            return afterNewKey(probeFailure)
        }
    }

    /// Whether every channel has a verdict (nothing is owed).
    static func settled(_ outcomes: [Outcome]) -> Bool {
        !outcomes.contains(.retry)
    }
}

/// The channels a device created, and the key generation their owner key was
/// registered under (``ChannelsUiState/ownerKeyGenerationByChannel``), read
/// and written outside a live ``ChannelsUiState``: the handover runs from the
/// composition root, whether or not the channel screens are up.
enum OwnedChannelGenerations {

    static func key(_ accountIdB64: String) -> String {
        "lakko.channels.owner-generation.\(accountIdB64)"
    }

    static func load(accountIdB64: String, defaults: UserDefaults = .standard) -> [String: UInt64] {
        (defaults.dictionary(forKey: key(accountIdB64)) as? [String: NSNumber])?
            .mapValues { $0.uint64Value } ?? [:]
    }

    /// Record that the owner key of `channelIds` is registered under
    /// `generation` now, and tell a live state to re-read (so the channel is
    /// not read as orphaned by a reset when the session's generation moves).
    static func record(
        channelIds: [String], generation: UInt64, accountIdB64: String,
        defaults: UserDefaults = .standard
    ) {
        guard !channelIds.isEmpty else { return }
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        var map = load(accountIdB64: accountIdB64, defaults: defaults)
        for id in channelIds { map[id] = generation }
        defaults.set(map.mapValues { NSNumber(value: $0) }, forKey: key(accountIdB64))
        NotificationCenter.default.post(name: changed, object: nil, userInfo: ["accountIdB64": accountIdB64])
    }

    static let changed = Notification.Name("me.lakko.channels.owner-generation.changed")
}

/// 2026-11-20 — an owner device whose own channel refuses its owner proof
/// (`401 owner-proof-required`) moves the channel's owner key to the key its
/// CURRENT seed derives, signed by an own predecessor's key (ADR-0074
/// section 3.5), instead of waiting for a handover that may never come.
///
/// Seen live: an iPhone's reseals were refused for hours. The service holds
/// one owner key per channel, set at creation and moved only by a handover;
/// this device signs with its current seed's key. They part when the account
/// rotated and the handover for that channel never landed: a kept device's
/// handover is tried once and not owed, a device hands over only the
/// channels it created itself, and the web client signed with the lineage
/// key (fixed the same day). Running the handover from every own predecessor
/// heals each of those cases from any owner device.
///
/// One try per channel per process (a refusal hold lasts as long); a try
/// with no verdict (offline) may run again at the next pass.
@MainActor
enum ChannelOwnerRepair {

    private static var tried: Set<String> = []

    static func reset() { tried = [] }

    /// The predecessors to try, newest first: the key the service most
    /// likely holds is the one the last rotation left.
    nonisolated static func candidates(predecessorsOldestFirst: [Data], currentSeed: Data) -> [Data] {
        var seen: Set<Data> = [currentSeed]
        var out: [Data] = []
        for seed in predecessorsOldestFirst.reversed() where !seen.contains(seed) {
            seen.insert(seed)
            out.append(seed)
        }
        return out
    }

    /// What one channel's repair came to over the predecessors' handovers:
    /// owned at the first `.owned`; `.retry` when any handover got no
    /// verdict and none owned; `.notOwner` otherwise.
    static func repair(
        predecessors: [Data], currentSeed: Data,
        put: (_ signing: Identity, _ naming: Identity) async throws -> Void
    ) async -> ChannelOwnerHandover.Outcome {
        guard let new = try? Identity.fromSeed(seed: currentSeed) else { return .retry }
        var sawRetry = false
        for seed in candidates(predecessorsOldestFirst: predecessors, currentSeed: currentSeed) {
            guard let old = try? Identity.fromSeed(seed: seed) else { continue }
            switch await ChannelOwnerHandover.handOver(old: old, new: new, put: put) {
            case .owned: return .owned
            case .retry: sawRetry = true
            case .notOwner: continue
            }
        }
        return sawRetry ? .retry : .notOwner
    }

    /// Whether any of this device's channel retries holds `channelId` as
    /// refused.
    static func isRefused(accountIdB64: String, channelId: String) -> Bool {
        PublisherCommitRetry.shared.hold(accountIdB64: accountIdB64, channelIdHex: channelId)?.refused == true
            || SubscriberAdmission.retry.hold(accountIdB64: accountIdB64, channelIdHex: channelId)?.refused == true
            || ChannelAccountReaderKey.isRefused(channelId: channelId)
    }

    /// Repair `channelId` if a retry holds it as refused and this process
    /// has not tried yet. On `.owned` every hold of the channel goes, so the
    /// next pass sends what was held.
    static func repairIfRefused(
        channelId: String, accountIdB64: String, currentSeed: Data, client: ChannelsClient
    ) async {
        let key = accountIdB64 + "|" + channelId.lowercased()
        guard !tried.contains(key), isRefused(accountIdB64: accountIdB64, channelId: channelId) else { return }
        tried.insert(key)
        let predecessors = KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64)
        guard !predecessors.isEmpty else {
            NSLog("[channels] an owner proof is refused and the account has no earlier keys; nothing to hand over")
            return
        }
        let outcome = await repair(predecessors: predecessors, currentSeed: currentSeed) { signing, naming in
            try await client.replaceOwner(channelIdHex: channelId, signingIdentity: signing, newOwnerIdentity: naming)
        }
        switch outcome {
        case .owned:
            ChannelPublisherRoleCache.shared.clearAll()
            PublisherCommitRetry.shared.noteSuccess(accountIdB64: accountIdB64, channelIdHex: channelId)
            SubscriberAdmission.retry.noteSuccess(accountIdB64: accountIdB64, channelIdHex: channelId)
            ChannelAccountReaderKey.clearHold(channelId: channelId)
            NSLog("ADR-0074: a refused channel's owner key is now the current key's; held work runs again")
        case .retry:
            tried.remove(key)
            NSLog("[channels] the owner key repair got no answer; tried again next pass")
        case .notOwner:
            NSLog("[channels] no key of this account owns a refused channel; nothing to hand over")
        }
    }
}
