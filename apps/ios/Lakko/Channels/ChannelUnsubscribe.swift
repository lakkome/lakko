// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0060 section 3 — leaving a channel on the server. Before this there was
/// no way to leave short of the channels service's 90-day subscriber prune:
/// `DELETE /v1/channels/:id/subscribers/:pubkey_hex`, signed by the subscriber
/// key the path names (`unsubscribeRequestHeader`), removes the subscriber
/// row, its seed envelopes and its member card.
enum ChannelUnsubscribe {

    /// UX 9.5 — stop following `channelId` on this device: the server row, the
    /// keys, the row and its posts. The local state goes whatever the network
    /// does (a failure is logged). The keys are captured before they are
    /// forgotten: the request is signed by them. The posts leave the feed's
    /// log at its next sync, which keeps only the channels in the list.
    @MainActor
    static func stopFollowing(state: ChannelsUiState, channelId: String) {
        if let keys = state.subscriberKeysByChannel[channelId] {
            Task { await run(keys: keys, channelId: channelId) }
        }
        ChannelMemberCards.forgetUploads(channelId: channelId)
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        // ADR-0078 section 9 — the channel's members hold this account's one
        // `channel` key: the registration sees the channel gone and rotates it.
        ReachCenter.shared.sync()
    }

    /// ADR-0087 section 9 — the account's other devices said this channel
    /// ended. A channel this device publishes is not its to drop: unsubscribing
    /// is a subscriber's act.
    ///
    /// ADR-0099 step 5 — the exception is a channel moderators removed: its
    /// owner or publisher left it on another device, so it goes here too,
    /// once this device learns the service holds it removed (from what it
    /// already knows, else from one read, which a removed channel answers
    /// with `410 channel-removed`).
    @MainActor
    static func endedElsewhere(
        state: ChannelsUiState, channelIdHex: String,
        removals: ChannelRemovals = .shared,
        probeRemoved: @escaping @MainActor (String) async -> Bool = { id in
            do {
                _ = try await ChannelsClient().fetchEpoch(channelIdHex: id)
                return false
            } catch {
                return ChannelsApiError.isChannelRemoved(error)
            }
        }
    ) {
        guard let channel = state.channels.first(where: { $0.id.lowercased() == channelIdHex.lowercased() })
        else { return }
        guard state.keysByChannel[channel.id] != nil else {
            stopFollowing(state: state, channelId: channel.id)
            return
        }
        let id = channel.id
        if removals.isRemoved(id) {
            leaveRemoved(state: state, channelId: id)
            return
        }
        Task { @MainActor in
            guard await probeRemoved(id), state.channels.contains(where: { $0.id == id }) else { return }
            leaveRemoved(state: state, channelId: id)
        }
    }

    /// ADR-0099 step 5 — the owner or a publisher takes a channel moderators
    /// removed out of the list: the row, its posts and its keys go here; the
    /// channels service is not asked (it answers 410 for the channel). The
    /// caller tells the account's other devices.
    @MainActor
    static func leaveRemoved(state: ChannelsUiState, channelId: String) {
        ChannelMemberCards.forgetUploads(channelId: channelId)
        state.clearSubscriberKeys(channelId)
        state.removeOrphanedChannel(channelId)
        ReachCenter.shared.sync()
    }

    /// The signed request for leaving `channelId` with `keys`, stamped
    /// `issuedAtMs` (this device's clock; the server accepts it within its
    /// freshness window).
    static func request(
        keys: ChannelSubscriberKeys, channelId: String, issuedAtMs: UInt64,
        client: ChannelsClient = ChannelsClient()
    ) throws -> URLRequest {
        let (pubHex, header) = try signed(keys: keys, channelId: channelId, issuedAtMs: issuedAtMs)
        return try client.unsubscribeRequest(
            channelIdHex: channelId, subscriberPubHex: pubHex, sigHeader: header)
    }

    /// Leave `channelId` on the server. Call BEFORE the keys are forgotten:
    /// the request is signed by them. Never throws: a failure (offline, the
    /// server refused) is logged, and the caller drops its local state anyway;
    /// the server then forgets the subscriber at its 90-day prune.
    static func run(
        keys: ChannelSubscriberKeys, channelId: String,
        client: ChannelsClient = ChannelsClient()
    ) async {
        do {
            let (pubHex, header) = try signed(
                keys: keys, channelId: channelId,
                issuedAtMs: UInt64(max(0, Date().timeIntervalSince1970 * 1000)))
            try await client.unsubscribe(
                channelIdHex: channelId, subscriberPubHex: pubHex, sigHeader: header)
        } catch {
            NSLog("ADR-0060: channel unsubscribe not sent to the server: \(error)")
        }
    }

    private static func signed(
        keys: ChannelSubscriberKeys, channelId: String, issuedAtMs: UInt64
    ) throws -> (pubHex: String, header: String) {
        guard let channelIdBytes = CoPublisher.hexToData(channelId) else {
            throw ChannelsApiError(status: -1, code: "bad-channel-id", detail: "channel id is not hex")
        }
        let kp = try ChannelSubscriberKeypair.fromSecret(secret: keys.secret)
        let header = try kp.unsubscribeRequestHeader(channelId: channelIdBytes, issuedAtMs: issuedAtMs)
        return (ChannelsComposer.bytesToHex(kp.`public`()), header)
    }
}
