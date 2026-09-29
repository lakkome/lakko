// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// A channel's info layer, opened by tapping its name
/// (`docs/ux/conversation-info.md` sections 1 and 2.3).
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
    /// layer has nothing left to show.
    let onUnsubscribed: () -> Void
    let onDone: () -> Void

    @State private var confirmingStopFollowing = false

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
        NavigationStack {
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
                            Label("Manage channel", systemImage: "slider.horizontal.3")
                                .foregroundStyle(Color.lakkoText)
                        }
                    } footer: {
                        Text("Its name and description, who reads it, and who else may post.")
                    }
                }

                if sections.contains(.unsubscribe) {
                    Section {
                        Button(role: .destructive) { confirmingStopFollowing = true } label: {
                            Text("Stop following")
                        }
                    } footer: {
                        // Deliberately about THIS DEVICE. `services/channels`
                        // has no route that retires a subscriber, so the copy
                        // must never claim the channel has been told.
                        Text(
                            "Takes the channel off your list and stops this device "
                                + "reading it. You can subscribe again later."
                        )
                    }
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Channel info")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done", action: onDone)
                }
            }
            .confirmationDialog(
                "Stop following \(title)?",
                isPresented: $confirmingStopFollowing,
                titleVisibility: .visible
            ) {
                Button("Stop following", role: .destructive) { stopFollowing() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(
                    "It leaves your list and this device stops reading it. "
                        + "You can subscribe again later."
                )
            }
        }
    }

    /// The same two calls web's `confirmStopFollowing` makes, in the same
    /// order: forget the reader keypair and the seeds it recovered, then drop
    /// the channel from the local list.
    private func stopFollowing() {
        // ADR-0058 section 3 — the member card goes with the subscription;
        // signed by the subscriber key, so taken before the key is forgotten.
        if let keys = state.subscriberKeysByChannel[channelId] {
            let id = channelId
            Task { await ChannelMemberCards.delete(keys: keys, channelId: id) }
        }
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        onUnsubscribed()
    }
}
