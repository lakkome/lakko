// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

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

/// The Reactions screen for a channel, over its own setting model.
struct ChannelReactionsScreen: View {
    @StateObject private var setting: ChannelReactionsSetting
    @Environment(\.dismiss) private var dismiss

    init(channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data) {
        _setting = StateObject(wrappedValue: ChannelReactionsSetting(
            channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed))
    }

    var body: some View {
        AllowedReactionsScreen(viewModel: setting) { dismiss() }
    }
}
