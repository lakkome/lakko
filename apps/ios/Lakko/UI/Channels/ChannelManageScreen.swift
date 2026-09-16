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
                SubscribersPanel(
                    channelId: channelId,
                    state: state,
                    client: client
                )
                CoPublishersPanel(
                    channelId: channelId,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
                RotationPanel(
                    channelId: channelId,
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
