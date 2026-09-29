// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
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

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                ChannelNamePanel(channelId: channelId, state: state, client: client)
                SubscribersPanel(
                    channelId: channelId,
                    state: state,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
                CoPublishersPanel(
                    channelId: channelId,
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
        .navigationTitle("Manage")
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

    @State private var name: String = ""
    @State private var description: String = ""
    @State private var seededFrom: SharedChannelMeta?
    @State private var busy = false
    @State private var error: String?

    private var channel: ChannelHandle? { state.channels.first(where: { $0.id == channelId }) }

    private var dirty: Bool {
        let current = channel.map { ($0.sharedMeta?.name ?? $0.localName, $0.displayDescription) } ?? ("", "")
        return name.trimmingCharacters(in: .whitespacesAndNewlines) != current.0
            || description.trimmingCharacters(in: .whitespacesAndNewlines) != current.1
    }

    var body: some View {
        ChannelManageCard(
            title: "Channel",
            subtitle: "Everyone who subscribes sees the name and description. The server never does."
        ) {
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .disabled(busy)
                .onChange(of: name) { _, value in
                    if value.count > ChannelMetaRecord.nameMaxChars {
                        name = String(value.prefix(ChannelMetaRecord.nameMaxChars))
                    }
                    if value.contains(where: \.isNewline) {
                        name = value.filter { !$0.isNewline }
                    }
                }
            TextField("Description (optional)", text: $description, axis: .vertical)
                .lineLimit(2...5)
                .textFieldStyle(.roundedBorder)
                .disabled(busy)
                .onChange(of: description) { _, value in
                    if value.count > ChannelMetaRecord.descriptionMaxChars {
                        description = String(value.prefix(ChannelMetaRecord.descriptionMaxChars))
                    }
                }
            HStack {
                Spacer()
                Button(busy ? "Saving..." : "Save") { Task { await save() } }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
                    .disabled(busy || !dirty)
            }
            if let error {
                ChannelManageErrorLine(text: error)
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
        defer { busy = false }
        guard let meta = ChannelMetaRecord.fresh(
            name: name, description: description,
            nowMs: UInt64(Date().timeIntervalSince1970 * 1000))
        else {
            error = "A channel needs a name"
            return
        }
        do {
            try await ChannelMetaRecord.post(state: state, client: client, channelId: channelId, meta: meta)
            // Keep the local copy aligned so a device that never reads its own
            // record back (the publisher) still titles from what it sent.
            state.updateLocalMeta(
                channelId: channelId, localName: meta.name, localDescription: meta.description)
            seededFrom = meta
        } catch let err as LegacyChannelError {
            error = err.message
        } catch {
            NSLog("Channels: metadata record post failed: %@", String(describing: error))
            self.error = "Could not save. Check your connection and try again."
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
