// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0066 section 3.5, "Channels" — after a rotation of the account keys the
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
        var map = load(accountIdB64: accountIdB64, defaults: defaults)
        for id in channelIds { map[id] = generation }
        defaults.set(map.mapValues { NSNumber(value: $0) }, forKey: key(accountIdB64))
        NotificationCenter.default.post(name: changed, object: nil, userInfo: ["accountIdB64": accountIdB64])
    }

    static let changed = Notification.Name("me.lakko.channels.owner-generation.changed")
}
