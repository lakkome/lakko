// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// "List in Discover" on the channel manage screen (`docs/ux/feed.md` 6.4,
/// ADR-0059). Mirror of web `discover/ListInDiscover.tsx`.
///
/// The switch is the OWNER's: the listing is signed by a key only the account
/// that created the channel derives. A co-publisher sees the state and no
/// switch. Whether this account is the owner is asked of the server once (an
/// owner-signed read it refuses to a co-publisher).
struct ListInDiscoverPanel: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @State private var owner: Bool?
    @State private var listed: Bool?
    @State private var topic: UInt8 = 0
    @State private var language: String = DiscoverLanguages.appLanguage()
    @State private var busy = false
    @State private var notice: String?
    @State private var confirming = false
    @State private var seeded = false

    private let topics = channelTopics()

    private var channel: ChannelHandle? { state.channels.first { $0.id == channelId } }
    private var hasHandle: Bool {
        !(channel?.handle ?? "").trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" }).isEmpty
    }

    var body: some View {
        ChannelManageCard(title: "List in Discover") {
            if owner == true {
                ownerControls
            } else {
                Text(stateLine)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let notice {
                Text(notice)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task { await load() }
        .alert("List this channel in Discover?", isPresented: $confirming) {
            Button("Cancel", role: .cancel) {}
            Button("List") { Task { await turnOn() } }
        } message: {
            Text(ChannelListing.confirm)
        }
    }

    private var stateLine: String {
        switch listed {
        case .some(true): return "This channel is listed in Discover. Only the channel's owner can change this."
        case .some(false): return "This channel is not listed in Discover. Only the channel's owner can list it."
        case .none: return "Checking whether this channel is listed..."
        }
    }

    @ViewBuilder
    private var ownerControls: some View {
        Text("Anyone can find a listed channel's name, description and subscriber count. Posts stay encrypted.")
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .fixedSize(horizontal: false, vertical: true)
        Toggle(listed == true ? "Listed" : "Not listed", isOn: Binding(
            get: { listed == true },
            set: { on in
                if on {
                    notice = nil
                    if hasHandle { confirming = true } else { notice = ChannelListing.needsHandle }
                } else {
                    Task { await turnOff() }
                }
            }))
            .disabled(busy || listed == nil)
        if listed != true {
            Picker("Topic", selection: $topic) {
                ForEach(topics, id: \.id) { t in
                    Text(t.name).tag(t.id)
                }
            }
            .disabled(busy)
            Picker("Language", selection: $language) {
                ForEach(DiscoverLanguages.options(language), id: \.self) { code in
                    Text(DiscoverLanguages.label(code)).tag(code)
                }
            }
            .disabled(busy)
        }
        if !hasHandle {
            Text(ChannelListing.needsHandle)
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
        }
    }

    private func load() async {
        if !seeded, let meta = channel?.sharedMeta {
            seeded = true
            topic = meta.topic
            if !meta.language.isEmpty { language = meta.language }
        }
        do {
            listed = try await ChannelListing.listedStamp(client: client, channelIdHex: channelId) != nil
        } catch {
            listed = nil
        }
        guard owner == nil, let identity = try? Identity.fromSeed(seed: accountIdentitySeed) else { return }
        owner = await client.probeChannelOwner(channelIdHex: channelId, accountIdentity: identity)
    }

    private func turnOn() async {
        guard !busy, let identity = try? Identity.fromSeed(seed: accountIdentitySeed) else { return }
        busy = true
        listed = true
        defer { busy = false }
        do {
            try await ChannelListing.list(
                state: state, client: client, channelIdHex: channelId, identity: identity,
                topic: topic, language: language)
        } catch {
            // The switch returns to off when the server refused the listing.
            let live = (try? await ChannelListing.listedStamp(client: client, channelIdHex: channelId)) ?? nil
            listed = live != nil
            if live != nil {
                notice = "Listed. Subscribers will learn it with the next channel update."
            } else if (error as? ChannelListing.ListingError) == .needsHandle {
                notice = ChannelListing.needsHandle
            } else {
                notice = ChannelListing.refused
            }
        }
    }

    private func turnOff() async {
        guard !busy, let identity = try? Identity.fromSeed(seed: accountIdentitySeed) else { return }
        busy = true
        notice = nil
        defer { busy = false }
        do {
            try await ChannelListing.unlist(
                state: state, client: client, channelIdHex: channelId, identity: identity)
            listed = false
        } catch {
            let live = (try? await ChannelListing.listedStamp(client: client, channelIdHex: channelId)) ?? nil
            listed = live != nil
            notice = live != nil ? "Couldn't remove the listing. Try again." : nil
        }
    }
}
