// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The owner's side of Discover (ADR-0059 sections 2-3, `docs/ux/feed.md`
/// 6.4): list a channel, withdraw it, and keep the listing in step with a
/// rename. Mirror of web `discover/listing.ts`.
///
/// Listing writes two things, in this order:
///
///   1. the public listing (`PUT /v1/channels/{id}/listing`, owner-signed):
///      if the server refuses it the switch goes back to off and nothing else
///      happens ("Couldn't list this channel. Try again.");
///   2. channel metadata v2 with `listed = true` and the topic and language,
///      an ordinary encrypted post, so subscribers learn the channel is
///      listed (what lets them share it in a group, section 6).
///
/// Withdrawing deletes the listing and posts metadata with `listed = false`.
@MainActor
enum ChannelListing {
    /// The UX spec's copy, from the catalog (ADR-0061).
    static var refused: String { Localization.text("discover.listing.refused") }
    static var confirm: String { Localization.text("discover.listing.confirm_body") }
    static var needsHandle: String { Localization.text("discover.listing.needs_handle") }

    enum ListingError: Error, Equatable {
        case needsHandle
    }

    /// What the server currently lists for a channel: its stamp, or `nil`.
    static func listedStamp(client: ChannelsClient, channelIdHex: String) async throws -> UInt64? {
        guard let item = try await client.fetchListing(channelIdHex: channelIdHex) else { return nil }
        guard let bytes = Data(base64UrlNoPadListing: item.listingB64),
              let record = try? decodeChannelListing(bytes: bytes)
        else {
            // A listing this client cannot decode is still a listing.
            return 0
        }
        return record.updatedAtMs
    }

    /// A stamp strictly after the stored one.
    static func nextStamp(nowMs: Int64, stored: UInt64?) -> UInt64 {
        let now = UInt64(max(0, nowMs))
        guard let stored, now <= stored else { return now }
        return stored &+ 1
    }

    /// The name, description and handle the listing publishes.
    private static func source(state: ChannelsUiState, channelIdHex: String)
        -> (handle: String, name: String, description: String)
    {
        let channel = state.channels.first { $0.id == channelIdHex }
        let meta = channel?.sharedMeta
        let handle = (channel?.handle ?? "").trimmingCharacters(in: .whitespaces)
            .drop(while: { $0 == "@" })
        return (
            String(handle),
            (meta?.name ?? channel?.localName ?? "").trimmingCharacters(in: .whitespacesAndNewlines),
            (meta?.description ?? channel?.localDescription ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }

    private static func putCurrent(
        state: ChannelsUiState, client: ChannelsClient, channelIdHex: String,
        identity: Identity, topic: UInt8, language: String, stored: UInt64?
    ) async throws {
        let src = source(state: state, channelIdHex: channelIdHex)
        guard !src.handle.isEmpty else { throw ListingError.needsHandle }
        let bytes = try composeChannelListing(listing: FfiChannelListing(
            channelId: try ChannelsComposer.channelIdFromHex(channelIdHex),
            handle: src.handle,
            name: src.name,
            description: src.description,
            topic: topic,
            language: language,
            updatedAtMs: nextStamp(nowMs: Int64(Date().timeIntervalSince1970 * 1000), stored: stored)))
        try await client.putListing(channelIdHex: channelIdHex, accountIdentity: identity, listing: bytes)
    }

    /// The channel's metadata as held, with the Discover fields replaced.
    private static func metaWith(
        state: ChannelsUiState, channelIdHex: String, topic: UInt8?, language: String?, listed: Bool
    ) -> SharedChannelMeta? {
        let src = source(state: state, channelIdHex: channelIdHex)
        let held = state.sharedMeta(channelId: channelIdHex)
        return ChannelMetaRecord.fresh(
            name: src.name, description: src.description,
            nowMs: max(UInt64(Date().timeIntervalSince1970 * 1000), (held?.updatedAtMs ?? 0) &+ 1),
            keeping: SharedChannelMeta(
                name: src.name, description: src.description, updatedAtMs: 0,
                topic: topic ?? held?.topic ?? 0,
                language: language ?? held?.language ?? "",
                listed: listed))
    }

    /// List the channel: the listing PUT first (a refusal throws, nothing
    /// else happens), then the metadata post with `listed = true`.
    static func list(
        state: ChannelsUiState, client: ChannelsClient, channelIdHex: String,
        identity: Identity, topic: UInt8, language: String
    ) async throws {
        let stored = try? await listedStamp(client: client, channelIdHex: channelIdHex)
        try await putCurrent(
            state: state, client: client, channelIdHex: channelIdHex, identity: identity,
            topic: topic, language: language, stored: stored ?? nil)
        guard let meta = metaWith(
            state: state, channelIdHex: channelIdHex, topic: topic, language: language, listed: true)
        else { return }
        try await ChannelMetaRecord.post(
            state: state, client: client, channelId: channelIdHex, meta: meta, accountIdentity: identity)
    }

    /// Withdraw the listing and tell subscribers the channel is no longer
    /// listed.
    static func unlist(
        state: ChannelsUiState, client: ChannelsClient, channelIdHex: String, identity: Identity
    ) async throws {
        try await client.deleteListing(channelIdHex: channelIdHex, accountIdentity: identity)
        guard let meta = metaWith(
            state: state, channelIdHex: channelIdHex, topic: nil, language: nil, listed: false)
        else { return }
        try await ChannelMetaRecord.post(
            state: state, client: client, channelId: channelIdHex, meta: meta, accountIdentity: identity)
    }

    /// After a rename (ADR-0051 edit) of a listed channel: re-put the listing
    /// so it carries the new name and description. No-op when not listed.
    static func refreshAfterEdit(
        state: ChannelsUiState, client: ChannelsClient, channelIdHex: String, identity: Identity
    ) async throws {
        guard let meta = state.sharedMeta(channelId: channelIdHex), meta.listed, !meta.language.isEmpty
        else { return }
        guard let stored = try await listedStamp(client: client, channelIdHex: channelIdHex) else { return }
        try await putCurrent(
            state: state, client: client, channelIdHex: channelIdHex, identity: identity,
            topic: meta.topic, language: meta.language, stored: stored)
    }
}

extension Data {
    /// Decode a base64url string (padding optional). `nil` on any failure.
    init?(base64UrlNoPadListing s: String) {
        var std = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        std.append(String(repeating: "=", count: (4 - std.count % 4) % 4))
        guard let data = Data(base64Encoded: std) else { return nil }
        self = data
    }
}
