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
