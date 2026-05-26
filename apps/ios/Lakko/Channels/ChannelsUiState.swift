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

    public init() {}

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

    public func reset() {
        channels = []
        postsByChannel = [:]
        epochByChannel = [:]
        keysByChannel = [:]
        counterByChannel = [:]
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
