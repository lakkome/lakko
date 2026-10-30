// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// A channel's info, opened by tapping its name
/// (`docs/ux/conversation-info.md` sections 1 and 2.3): a screen pushed over
/// the channel, with the system back returning to it (section 3.1).
///
/// A channel used to have no info layer on iOS at all. The argument for that
/// was that ``ChannelDetailScreen`` is "both its conversation and its info",
/// and it stopped holding the moment the other two clients gave a channel a
/// real one reached by its name: this device showed no handle, no description
/// block, no subscriber count and — the actual defect — no way to stop
/// following. `ChannelsUiState.removeChannel(_:)` and
/// `clearSubscriberKeys(_:)` had been sitting here the whole time with no
/// caller anywhere.
///
/// What it holds is `channelInfoSections` in `crates/lakko-ui-rules`, shared
/// with web and Android. Notably `Unsubscribe` is for a subscriber and NOT
/// for a publisher: a publisher reads its own channel from its own publisher
/// keys, so there is no subscription to end and a control offering to end one
/// would end nothing.
struct ChannelInfoSheet: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// ADR-0028 OQ3 — threaded to the manage screen, which opens the
    /// channel's PublisherGroup and signs.
    let accountIdentitySeed: Data
    /// The handle the channel was found by when it is not listed yet.
    var foundHandle: String = ""
    /// The channel has left this device's list and the screen behind this
    /// one has nothing left to show.
    let onUnsubscribed: () -> Void

    @State private var confirmingStopFollowing = false

    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    private var channel: ChannelHandle? {
        state.channels.first { $0.id == channelId }
    }

    private var isPublisher: Bool { state.keysByChannel[channelId] != nil }

    private var subscriberCount: Int? { state.subscriberCountByChannel[channelId] }

    /// Section 2.3, decided once in the shared rule rather than by flags read
    /// in this layout. The subscriber count is offered only when this device
    /// HAS one — only a publisher ever lists subscribers, and a reader
    /// inventing a number it cannot know would be worse than showing none.
    private var sections: [InfoSection] {
        channelInfoSections(
            caps: ChannelInfoCapabilities(
                isPublisher: isPublisher,
                isSubscriber: state.subscriberKeysByChannel[channelId] != nil,
                subscriberCountKnown: subscriberCount != nil))
    }

    private var handle: String { channel?.handle ?? foundHandle }

    private var title: String {
        ChannelsFormatting.title(
            sharedName: channel?.sharedName ?? "",
            localName: channel?.localName ?? "",
            handle: handle)
    }

    /// `@handle`, never repeated when the name IS the handle.
    private var handleLine: String? {
        let bare = ChannelsFormatting.bareHandle(handle)
        return !bare.isEmpty && bare != title ? "@\(bare)" : nil
    }

    private var description: String {
        (channel?.displayDescription ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        List {
                Section {
                    VStack(spacing: 8) {
                        InitialsAvatar(
                            name: title,
                            identity: channelId,
                            size: 72,
                            placeholderSymbol: "megaphone.fill"
                        )
                        Text(title)
                            .font(.title2.weight(.semibold))
                            .foregroundStyle(Color.lakkoText)
                            .multilineTextAlignment(.center)
                        if sections.contains(.channelHandle), let handleLine {
                            Text(handleLine)
                                .font(.subheadline)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                        if sections.contains(.subscriberCount), let subscriberCount {
                            Text(ChannelsFormatting.subscriberCountLabel(subscriberCount))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
                    .listRowBackground(Color.clear)
                }

                if sections.contains(.channelDescription), !description.isEmpty {
                    Section {
                        Text(description)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoText)
                    }
                }

                if sections.contains(.manageChannel) {
                    Section {
                        NavigationLink {
                            ChannelManageScreen(
                                channelId: channelId,
                                state: state,
                                client: client,
                                accountIdentitySeed: accountIdentitySeed)
                        } label: {
                            Label(l10n.t("channels.manage.title"), systemImage: "slider.horizontal.3")
                                .foregroundStyle(Color.lakkoText)
                        }
                    } footer: {
                        Text(l10n.t("channels.info.manage_sub"))
                    }
                }

                if sections.contains(.unsubscribe) {
                    Section {
                        Button(role: .destructive) { confirmingStopFollowing = true } label: {
                            Text(l10n.t("channels.unsubscribe.action"))
                        }
                    } footer: {
                        // Deliberately about THIS DEVICE: the subscription is
                        // this device's subscriber key. ADR-0060 section 3's
                        // signed DELETE retires it on the server too, but best
                        // effort, so the copy still claims nothing about the
                        // channel having been told. Same words as web.
                        Text(l10n.t("channels.info.unsubscribe_sub"))
                    }
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(l10n.t("chat.info.channel"))
            .navigationBarTitleDisplayMode(.inline)
            .lakkoDialog(
                isPresented: $confirmingStopFollowing,
                title: l10n.t("channels.unsubscribe.title_named", texts: ["name": title]),
                message: l10n.t("channels.unsubscribe.body")
            ) {
                [.danger(l10n.t("channels.unsubscribe.action")) { stopFollowing() }, .cancel()]
            }
    }

    /// The same two calls web's `confirmStopFollowing` makes, in the same
    /// order: forget the reader keypair and the seeds it recovered, then drop
    /// the channel from the local list.
    private func stopFollowing() {
        // ADR-0060 section 3 — leave on the server too (the subscriber row,
        // its seed envelopes and its member card). Signed by the subscriber
        // key, so the keys are captured before they are forgotten; the local
        // state goes whatever the network does (a failure is logged).
        if let keys = state.subscriberKeysByChannel[channelId] {
            let id = channelId
            Task { await ChannelUnsubscribe.run(keys: keys, channelId: id) }
        }
        ChannelMemberCards.forgetUploads(channelId: channelId)
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        // ADR-0078 section 9 — the channel's members hold this account's one
        // `channel` key: the registration sees the channel gone and rotates it.
        ReachCenter.shared.sync()
        onUnsubscribed()
    }
}
