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
    @State private var removing: Removal?
    @State private var showBanned = false

    enum Phase: Equatable { case loading, loaded, failed }

    private var isPublisher: Bool { state.keysByChannel[channelId] != nil }
    private var isPrivate: Bool { state.extras(channelId).isPublic == false }
    private var count: Int? { state.subscriberCountByChannel[channelId] }

    /// Section 15.1 — the list through the shared rule
    /// (`lakko_ui_rules::subscribers`): named rows, the others and their
    /// count, so the owner can always tell whom `Remove` takes.
    private var list: SubscriberList {
        SubscriberList.build(
            members: members, rows: rows, count: count, canRemove: isPrivate && isPublisher,
            excludedKeys: SubscriberCount.ownPublisherKeys(publishes: isPublisher, keys: [
                state.extras(channelId).readerKeyRegisteredHex,
                ChannelAccountReaderKey.keyHex(channelId: channelId),
                state.subscriberKeysByChannel[channelId]
                    .flatMap { try? ChannelSubscriberKeypair.fromSecret(secret: $0.secret).`public`() }
                    .map { ChannelsComposer.bytesToHex($0) },
            ] + PublisherReaderKeys.otherKeys(channelId: channelId, state: state).map { Optional($0) }))
    }

    var body: some View {
        List {
            if phase == .failed {
                Section {
                    Text(l10n.t("channels.subscribers.load_failed")).foregroundStyle(Color.lakkoDanger)
                    Button(l10n.t("common.try_again")) { Task { await load() } }
                }
            } else if phase == .loaded, list.people.isEmpty, (count ?? 0) == 0, list.others.isEmpty {
                Section {
                    Text(l10n.t("channels.subscribers.none")).foregroundStyle(Color.lakkoTextMuted)
                    if let onInvitePeople {
                        Button(l10n.t("channels.info.invite_people"), action: onInvitePeople)
                    }
                }
            } else {
                let list = self.list
                Section {
                    ForEach(list.people, id: \.member.accountKey) { person in
                        let member = person.member
                        HStack(spacing: 12) {
                            InitialsAvatar(
                                name: member.claim.displayName ?? member.claim.username ?? "",
                                identity: member.subscriberPk.base64EncodedString(), size: 40,
                                photo: member.claim.avatar.map(AvatarPhoto.avatar))
                            VStack(alignment: .leading, spacing: 2) {
                                if member.isSelf {
                                    Text(l10n.t("channels.members.you")).foregroundStyle(Color.lakkoText)
                                } else if let name = member.claim.displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
                                   !name.isEmpty {
                                    Text(name).foregroundStyle(Color.lakkoText)
                                    if let username = member.claim.username {
                                        Text("@" + username)
                                            .font(.footnote)
                                            .foregroundStyle(Color.lakkoTextMuted)
                                            .environment(\.layoutDirection, .leftToRight)
                                    }
                                } else if let username = member.claim.username {
                                    Text("@" + username)
                                        .foregroundStyle(Color.lakkoText)
                                        .environment(\.layoutDirection, .leftToRight)
                                }
                            }
                            if person.removable {
                                Spacer()
                                Button(l10n.t("common.remove"), role: .destructive) {
                                    removing = Removal(pubkeysHex: person.pubkeysHex)
                                }
                                .buttonStyle(.borderless)
                            }
                        }
                    }
                    if list.othersCount > 0 {
                        Text(l10n.t("channels.subscribers.others", counts: ["count": UInt64(list.othersCount)]))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    // Section 15.1 — in a private channel the others, each
                    // with `Remove`.
                    ForEach(list.others, id: \.pubkeyHex) { row in
                        HStack {
                            Text(linkLabel(row))
                                .foregroundStyle(Color.lakkoText)
                            Spacer()
                            Button(l10n.t("common.remove"), role: .destructive) {
                                removing = Removal(pubkeysHex: [row.pubkeyHex])
                            }
                            .buttonStyle(.borderless)
                        }
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
        .lakkoDialog(item: $removing) { removal in
            LakkoDialogContent(
                title: l10n.t("channels.subscribers.remove_title"),
                message: l10n.t("channels.subscribers.remove_body"), field: nil,
                actions: [.danger(l10n.t("common.remove")) { Task { await remove(removal) } }, .cancel()])
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
            state.setServiceSubscriberCount(channelId: channelId, serviceCount: exact)
        }
        members = await ChannelMemberCards.refreshMembers(
            state: state, channelId: channelId, includeSelf: true,
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

    /// What `Remove` takes: every subscriber key of the person (one per
    /// device), or one key of an unnamed row.
    struct Removal: Identifiable {
        let pubkeysHex: [String]
        var id: String { pubkeysHex.joined(separator: ",") }
    }

    /// Section 7.3 — remove a subscriber of a private channel; a rotation
    /// follows at once on the owner's device (a publisher asks it to).
    private func remove(_ removal: Removal) async {
        guard let identity = try? Identity.fromSeed(seed: accountIdentitySeed) else { return }
        var gone = Set<String>()
        for pubkeyHex in removal.pubkeysHex {
            do {
                try await client.removeSubscriber(
                    channelIdHex: channelId, pubkeyHex: pubkeyHex, accountIdentity: identity)
                gone.insert(pubkeyHex.lowercased())
            } catch {
                NSLog("[channels] a subscriber was not removed")
                break
            }
        }
        guard !gone.isEmpty else { return }
        rows.removeAll { gone.contains($0.pubkeyHex.lowercased()) }
        members = members.compactMap { member in
            var kept = member
            kept.subscriberPks.removeAll { gone.contains(ChannelsComposer.bytesToHex($0).lowercased()) }
            return kept.subscriberPks.isEmpty && !kept.isSelf ? nil : kept
        }
        if let count {
            state.setSubscriberCount(channelId: channelId, count: SubscriberCount.afterRemoval(count, removed: gone.count))
        }
        // Only the owner rotates; a publisher's removal asks the owner's
        // device to (ADR-0087 section 15, the rotate request).
        await ChannelPublisherLoops.shared.rotateAfterSubscriberRemoval(
            channelId: channelId, channels: state, client: client, seed: accountIdentitySeed)
    }
}

/// Section 15.1 / 7.3 — a channel's subscriber list as a person sees it, built
/// by the shared rule (`lakko_ui_rules::subscribers`): a card that carries no
/// name is not a named row; a person shown by name is never listed again
/// among the others; in a private channel the owner and publishers may remove
/// every row. Base walk 2026-11-13: a nameless card read "Member", and the
/// owner could not tell whom `Remove` took.
struct SubscriberList {
    struct Person {
        let member: ChannelMemberCards.Member
        /// Every subscriber key of this person (hex): what `Remove` takes.
        let pubkeysHex: [String]
        let removable: Bool
    }

    let people: [Person]
    /// The others listed one row each (private channel, owner, publishers).
    let others: [ChannelsClient.SubscriberRow]
    /// The number in "{n} others."; `0` says nothing.
    let othersCount: Int

    /// `excludedKeys`: keys that are not subscribers, the publishing
    /// viewer's own (``SubscriberCount/ownPublisherKeys(publishes:keys:)``),
    /// left out of the cards and the listed rows; `count` must already leave
    /// them out (``SubscriberCount/shown(serviceCount:publisherReaderRows:)``).
    static func build(
        members: [ChannelMemberCards.Member], rows: [ChannelsClient.SubscriberRow],
        count: Int?, canRemove: Bool, excludedKeys: Set<String> = []
    ) -> SubscriberList {
        let rows = rows.filter { !excludedKeys.contains($0.pubkeyHex.lowercased()) }
        var cardMembers: [ChannelMemberCards.Member] = []
        var cardKeys: [String] = []
        var cards: [ChannelSubscriberCard] = []
        for member in members {
            let pks = member.subscriberPks.isEmpty ? [member.subscriberPk] : member.subscriberPks
            for pk in pks {
                let hex = ChannelsComposer.bytesToHex(pk).lowercased()
                if excludedKeys.contains(hex) { continue }
                cardMembers.append(member)
                cardKeys.append(hex)
                cards.append(ChannelSubscriberCard(
                    subscriberKey: hex, personKey: member.accountKey.base64EncodedString(),
                    named: member.named, isSelf: member.isSelf))
            }
        }
        let out = channelSubscriberList(
            cards: cards, listedKeys: rows.map { $0.pubkeyHex.lowercased() },
            count: count.map { UInt64(max(0, $0)) }, canRemove: canRemove)
        let people: [Person] = out.people.compactMap { person in
            guard let first = person.cards.first, Int(first) < cardMembers.count else { return nil }
            return Person(
                member: cardMembers[Int(first)],
                pubkeysHex: person.cards.compactMap { Int($0) < cardKeys.count ? cardKeys[Int($0)] : nil },
                removable: person.removable)
        }
        let others = out.others.compactMap { Int($0) < rows.count ? rows[Int($0)] : nil }
        return SubscriberList(
            people: people, others: others, othersCount: Int(clamping: out.othersCount))
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
