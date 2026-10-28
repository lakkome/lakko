// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// Manage (UX spec section 5.4): subscribers, co-publishers and access
/// refresh for a channel this device publishes to. Three cards, each owning
/// its own lifecycle exactly as it did inside the old detail screen
/// (``SubscribersPanel`` auto fan-out, ``CoPublishersPanel`` journal poll,
/// ``RotationPanel`` heartbeat).
struct ChannelManageScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// ADR-0028 OQ3 — the canonical AccountIdentity seed, so the co-publisher
    /// and rotation panels can open the channel's PublisherGroup + sign.
    let accountIdentitySeed: Data
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                ChannelNamePanel(
                    channelId: channelId, state: state, client: client,
                    accountIdentitySeed: accountIdentitySeed)
                // ADR-0059 — the owner's listing switch (docs/ux/feed.md 6.4).
                ListInDiscoverPanel(
                    channelId: channelId, state: state, client: client,
                    accountIdentitySeed: accountIdentitySeed)
                SubscribersPanel(
                    channelId: channelId,
                    state: state,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
                CoPublishersPanel(
                    channelId: channelId,
                    state: state,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
                RotationPanel(
                    channelId: channelId,
                    state: state,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("channels.manage.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - Name and description (ADR-0051 §4)

/// The channel's shared name and description, editable by the publisher.
/// Save posts a new metadata record into the channel (an ordinary encrypted,
/// signed post); every subscriber re-titles when it lands. An empty name is
/// refused ("A channel needs a name"); a failed post shows the error line and
/// leaves the fields as typed.
private struct ChannelNamePanel: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @State private var name: String = ""
    @State private var description: String = ""
    @State private var seededFrom: SharedChannelMeta?
    @State private var busy = false
    @State private var error: UiText?
    /// The fields as they were saved while `channels.meta.saved` is shown:
    /// set by a successful save, cleared by the next save or by an edit that
    /// makes either field differ from what was saved.
    @State private var savedFields: (name: String, description: String)?
    @ObservedObject private var l10n = Localization.shared

    private var channel: ChannelHandle? { state.channels.first(where: { $0.id == channelId }) }

    private var dirty: Bool {
        let current = channel.map { ($0.sharedMeta?.name ?? $0.localName, $0.displayDescription) } ?? ("", "")
        return name.trimmingCharacters(in: .whitespacesAndNewlines) != current.0
            || description.trimmingCharacters(in: .whitespacesAndNewlines) != current.1
    }

    var body: some View {
        ChannelManageCard(
            title: l10n.t("channels.meta.title"),
            subtitle: l10n.t("channels.meta.desc")
        ) {
            TextField(l10n.t("channels.field.name"), text: $name)
                .textFieldStyle(.roundedBorder)
                .disabled(busy)
                .onChange(of: name) { _, value in
                    if let saved = savedFields, value != saved.name { savedFields = nil }
                    if value.count > ChannelMetaRecord.nameMaxChars {
                        name = String(value.prefix(ChannelMetaRecord.nameMaxChars))
                    }
                    if value.contains(where: \.isNewline) {
                        name = value.filter { !$0.isNewline }
                    }
                }
            TextField(l10n.t("channels.field.description_optional"), text: $description, axis: .vertical)
                .lineLimit(2...5)
                .textFieldStyle(.roundedBorder)
                .disabled(busy)
                .onChange(of: description) { _, value in
                    if let saved = savedFields, value != saved.description { savedFields = nil }
                    if value.count > ChannelMetaRecord.descriptionMaxChars {
                        description = String(value.prefix(ChannelMetaRecord.descriptionMaxChars))
                    }
                }
            HStack {
                Spacer()
                Button(l10n.t(busy ? "common.saving" : "common.save")) { Task { await save() } }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
                    .disabled(busy || !dirty)
            }
            if let error {
                ChannelManageErrorLine(text: error.text)
            } else if savedFields != nil {
                Text(l10n.t("channels.meta.saved"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .onAppear { seed() }
        .onChange(of: channel?.sharedMeta) { _, _ in seed() }
    }

    /// Seed the fields from the newest record (else the local values), unless
    /// the user has already typed something different.
    private func seed() {
        guard let channel else { return }
        let shared = channel.sharedMeta
        let seededName = seededFrom?.name ?? ""
        let seededDescription = seededFrom?.description ?? ""
        if seededFrom == nil || (name == seededName && description == seededDescription) {
            name = shared?.name ?? channel.localName
            description = shared?.description ?? channel.localDescription
        }
        seededFrom = shared ?? SharedChannelMeta(
            name: channel.localName, description: channel.localDescription, updatedAtMs: 0)
    }

    private func save() async {
        if busy { return }
        busy = true
        error = nil
        savedFields = nil
        defer { busy = false }
        // ADR-0059 — a rename keeps the channel's Discover fields.
        guard let meta = ChannelMetaRecord.fresh(
            name: name, description: description,
            nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
            keeping: channel?.sharedMeta)
        else {
            error = .key("channels.meta.error.name_empty")
            return
        }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            try await ChannelMetaRecord.post(
                state: state, client: client, channelId: channelId, meta: meta, accountIdentity: identity)
            // Keep the local copy aligned so a device that never reads its own
            // record back (the publisher) still titles from what it sent.
            state.updateLocalMeta(
                channelId: channelId, localName: meta.name, localDescription: meta.description)
            seededFrom = meta
            savedFields = (name, description)
            // ADR-0059 — a listed channel's listing carries the name and
            // description too; editing them updates it (UX spec 6.4).
            if meta.listed {
                do {
                    try await ChannelListing.refreshAfterEdit(
                        state: state, client: client, channelIdHex: channelId, identity: identity)
                } catch {
                    self.error = .key("discover.listing.update_failed")
                }
            }
        } catch let err as LegacyChannelError {
            error = UiText { err.message }
        } catch {
            NSLog("Channels: metadata record post failed: %@", String(describing: error))
            self.error = ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.meta.error.save_prefix",
                fallbackKey: "channels.meta.error.save")
        }
    }
}

// MARK: - Card chrome shared by the three panels

/// The card / section component (design system section 3): elevated surface,
/// radius 12, hairline border, a section title and an optional muted line.
struct ChannelManageCard<Content: View>: View {
    let title: String
    var subtitle: String? = nil
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.headline)
                    .foregroundStyle(Color.lakkoText)
                if let subtitle {
                    Text(subtitle)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            content()
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }
}

/// The inline error line every panel shows under its rows.
struct ChannelManageErrorLine: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(Color.lakkoDanger)
            .fixedSize(horizontal: false, vertical: true)
    }
}
