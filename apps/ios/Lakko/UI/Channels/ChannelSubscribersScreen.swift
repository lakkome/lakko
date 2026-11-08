// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 15.1 — a channel's
/// subscribers: one row per person who shares a name, then "{n} others.".
/// In a private channel the owner and publishers also see the others, one row
/// each, with `Remove` (section 7.3). Never "Subscriber 1", "Ready", a retry
/// or anything about how a device got its access.
struct ChannelSubscribersScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    var onInvitePeople: (() -> Void)?

    @ObservedObject private var l10n = Localization.shared
    @State private var members: [ChannelMemberCards.Member] = []
    @State private var rows: [ChannelsClient.SubscriberRow] = []
    @State private var phase: Phase = .loading
    @State private var removing: ChannelsClient.SubscriberRow?
    @State private var showBanned = false

    enum Phase: Equatable { case loading, loaded, failed }

    private var isPublisher: Bool { state.keysByChannel[channelId] != nil }
    private var isPrivate: Bool { state.extras(channelId).isPublic == false }
    private var count: Int? { state.subscriberCountByChannel[channelId] }

    /// Section 15.1 — subscribers who share no name.
    private var othersCount: Int {
        max(0, (count ?? rows.count) - members.count)
    }

    private var unnamedRows: [ChannelsClient.SubscriberRow] {
        let named = Set(members.map { ChannelsComposer.bytesToHex($0.subscriberPk) })
        return rows.filter { !named.contains($0.pubkeyHex.lowercased()) }
    }

    var body: some View {
        List {
            if phase == .failed {
                Section {
                    Text(l10n.t("channels.subscribers.load_failed")).foregroundStyle(Color.lakkoDanger)
                    Button(l10n.t("common.try_again")) { Task { await load() } }
                }
            } else if phase == .loaded, members.isEmpty, (count ?? 0) == 0, rows.isEmpty {
                Section {
                    Text(l10n.t("channels.subscribers.none")).foregroundStyle(Color.lakkoTextMuted)
                    if let onInvitePeople {
                        Button(l10n.t("channels.info.invite_people"), action: onInvitePeople)
                    }
                }
            } else {
                Section {
                    ForEach(members, id: \.subscriberPk) { member in
                        HStack(spacing: 12) {
                            InitialsAvatar(
                                name: member.claim.displayName ?? "",
                                identity: member.subscriberPk.base64EncodedString(), size: 40,
                                photo: member.claim.avatar.map(AvatarPhoto.avatar))
                            VStack(alignment: .leading, spacing: 2) {
                                Text(member.claim.displayName ?? l10n.t("channels.members.unnamed"))
                                    .foregroundStyle(Color.lakkoText)
                                if let username = member.claim.username {
                                    Text("@" + username)
                                        .font(.footnote)
                                        .foregroundStyle(Color.lakkoTextMuted)
                                        .environment(\.layoutDirection, .leftToRight)
                                }
                            }
                        }
                    }
                    if isPrivate, isPublisher {
                        // Section 15.1 — the others, each with `Remove`.
                        ForEach(unnamedRows, id: \.pubkeyHex) { row in
                            HStack {
                                Text(linkLabel(row))
                                    .foregroundStyle(Color.lakkoText)
                                Spacer()
                                Button(l10n.t("common.remove"), role: .destructive) { removing = row }
                                    .buttonStyle(.borderless)
                            }
                        }
                    } else if othersCount > 0 {
                        Text(l10n.t("channels.subscribers.others", counts: ["count": UInt64(othersCount)]))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if phase == .loading { ProgressView() }
                }
                if isPublisher, !state.extras(channelId).bannedAuthors.isEmpty {
                    Section {
                        Button(l10n.t("channels.moderation.banned")) { showBanned = true }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(count.map {
            l10n.t("channels.subscribers.title_count", texts: ["count": $0.formatted(.number.locale(l10n.locale))])
        } ?? l10n.t("channels.subscribers.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .navigationDestination(isPresented: $showBanned) {
            ChannelBannedScreen(
                channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
        }
        .lakkoDialog(item: $removing) { row in
            LakkoDialogContent(
                title: l10n.t("channels.subscribers.remove_title"),
                message: l10n.t("channels.subscribers.remove_body"), field: nil,
                actions: [.danger(l10n.t("common.remove")) { Task { await remove(row) } }, .cancel()])
        }
    }

    /// Section 15.1 — the link a private subscriber came through, read from
    /// its ticket on this device: `From link "Friends"`, else (a contact
    /// invitation, or a link with no label) `From an invitation`.
    private func linkLabel(_ row: ChannelsClient.SubscriberRow) -> String {
        if let ticket = row.sealedTicket, let pk = Data(channelHex: row.pubkeyHex),
           let channelIdBytes = ChannelsClient.hexToData(channelId),
           let inviteKey = ChannelInvites.admissionSeeds(channelIdHex: channelId).reversed().lazy
               .compactMap({ try? openChannelAdmissionTicket(
                   admissionSeed: $0.seed, channelId: channelIdBytes, subscriberPk: pk, ticket: ticket) }).first,
           let link = ChannelInvites.links(channelIdHex: channelId).first(where: { $0.invitePublicKey == inviteKey }),
           !link.label.isEmpty {
            return l10n.t("channels.subscribers.from_link", texts: ["label": link.label])
        }
        return l10n.t("channels.subscribers.from_invitation")
    }

    private func load() async {
        phase = .loading
        let identity = try? Identity.fromSeed(seed: accountIdentitySeed)
        if isPublisher, let identity,
           let exact = try? await client.subscriberCount(channelIdHex: channelId, accountIdentity: identity) {
            state.setSubscriberCount(channelId: channelId, count: Int(clamping: exact))
        }
        members = await ChannelMemberCards.refreshMembers(
            state: state, channelId: channelId,
            resolveKey: { accountId in
                try? await ContactsClient().resolveVerifiedBundle(accountId: accountId).signaturePubkey
            })
        if isPrivate, isPublisher, let identity {
            var all: [ChannelsClient.SubscriberRow] = []
            var after: UInt64 = 0
            do {
                // A page at a time as the screen loads (section 15.1); capped
                // so a very large private channel does not block the screen.
                for _ in 0..<20 {
                    let page = try await client.subscriberPage(
                        channelIdHex: channelId, after: after, accountIdentity: identity)
                    all.append(contentsOf: page.rows)
                    guard !page.rows.isEmpty, page.nextAfter > after else { break }
                    after = page.nextAfter
                }
            } catch {
                phase = .failed
                return
            }
            rows = all
        }
        phase = .loaded
    }

    /// Section 7.3 — remove a subscriber of a private channel; a rotation
    /// follows at once on the owner's device (a publisher asks it to).
    private func remove(_ row: ChannelsClient.SubscriberRow) async {
        guard let identity = try? Identity.fromSeed(seed: accountIdentitySeed) else { return }
        do {
            try await client.removeSubscriber(channelIdHex: channelId, pubkeyHex: row.pubkeyHex, accountIdentity: identity)
            rows.removeAll { $0.pubkeyHex == row.pubkeyHex }
            if let count { state.setSubscriberCount(channelId: channelId, count: max(0, count - 1)) }
            // Only the owner rotates; a publisher's removal asks the owner's
            // device to (ADR-0087 section 15, the rotate request).
            await ChannelPublisherLoops.shared.rotateAfterSubscriberRemoval(
                channelId: channelId, channels: state, client: client, seed: accountIdentitySeed)
        } catch {
            NSLog("[channels] a subscriber was not removed")
        }
    }
}

/// Section 13.3 — `Banned`, the authors whose comments are hidden, with
/// `Unban`, for the owner and publishers. A comment author is known only by
/// a per-channel key and the name a comment carried.
struct ChannelBannedScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    @ObservedObject private var l10n = Localization.shared
    @StateObject private var model: BanModel

    init(channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        _model = StateObject(wrappedValue: BanModel(conversation: ChannelConversationModel(
            channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)))
    }

    @MainActor
    final class BanModel: ObservableObject {
        let conversation: ChannelConversationModel
        init(conversation: ChannelConversationModel) { self.conversation = conversation }
    }

    var body: some View {
        List {
            ForEach(state.extras(channelId).bannedAuthors, id: \.self) { key in
                HStack {
                    InitialsAvatar(name: "", identity: key.base64EncodedString(), size: 32)
                    Text(l10n.t("channels.moderation.banned"))
                        .foregroundStyle(Color.lakkoTextMuted)
                    Spacer()
                    Button(l10n.t("channels.moderation.unban")) {
                        Task { await model.conversation.unban(key) }
                    }
                    .buttonStyle(.borderless)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("channels.moderation.banned"))
        .navigationBarTitleDisplayMode(.inline)
    }
}
