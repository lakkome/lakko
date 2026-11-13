// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

/// `docs/ux/group-and-channel-privacy.md` section 14.1 — `Invite people`:
/// contacts only, the trailing `Invite` with the count. Each person gets the
/// invitation in their one-to-one; it waits for them to press Subscribe.
struct ChannelInvitePeopleScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    @ObservedObject var contacts: ContactsViewModel
    let chat: ChatRepository
    /// Done; the names the send failed for, for the info screen's line.
    let onDone: (_ failedNames: [String]) -> Void
    /// UX 15.2 — `Add publisher`: one pick, and the card is an invitation to
    /// publish (a one-use publisher link) instead of to subscribe.
    var publisher = false

    @ObservedObject private var l10n = Localization.shared
    @State private var selected: Set<Data> = []
    @State private var sending = false
    /// The account keys known to be in the channel (this account, the owner,
    /// the publishers and, for `Invite people`, subscribers by their card).
    @State private var inChannel: Set<Data> = []

    /// The contacts not known to be in the channel (sections 14.1, 15.2): the
    /// invitation is a message in their one-to-one, started if there is none,
    /// in the order Message uses.
    private var candidates: [AcceptedContact] {
        Self.candidates(contacts.repositoryState.accepted, inChannel: inChannel)
    }

    /// Sections 14.1 and 15.2 — `accepted` less every contact whose account
    /// key, current or earlier, is in `inChannel`. Pure.
    static func candidates(_ accepted: [AcceptedContact], inChannel: Set<Data>) -> [AcceptedContact] {
        guard !inChannel.isEmpty else { return accepted }
        return accepted.filter { entry in
            if inChannel.contains(entry.request.fromPubkey) { return false }
            return !(entry.previousKeysB64 ?? []).contains { Data(base64Encoded: $0).map(inChannel.contains) ?? false }
        }
    }

    /// The owner and the publishers, from the PublisherGroup's members, and
    /// this account; for `Invite people`, the subscribers whose card this
    /// device could open too.
    private func loadInChannel() async {
        var keys: Set<Data> = []
        if let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed) {
            keys.insert(identity.signaturePublicKey())
            if let group = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil,
               let members = try? group.members() {
                for member in members {
                    keys.insert(member.accountKey.isEmpty ? member.signaturePublicKey : member.accountKey)
                }
            }
        }
        if let own = try? Identity.fromSeed(seed: accountIdentitySeed) { keys.insert(own.signaturePublicKey()) }
        inChannel = keys
        guard !publisher else { return }
        let cards = await ChannelMemberCards.refreshMembers(
            state: state, channelId: channelId,
            resolveKey: { accountId in
                try? await ContactsClient().resolveVerifiedBundle(accountId: accountId).signaturePubkey
            })
        for card in cards where !card.accountKey.isEmpty { keys.insert(card.accountKey) }
        inChannel = keys
    }

    var body: some View {
        List {
            ForEach(candidates, id: \.request.fromPubkey) { entry in
                let isOn = selected.contains(entry.request.fromPubkey)
                ResolvedName(signatureKey: entry.request.fromPubkey) { resolved in
                    Button {
                        if isOn {
                            selected.remove(entry.request.fromPubkey)
                        } else if publisher {
                            selected = [entry.request.fromPubkey]
                        } else {
                            selected.insert(entry.request.fromPubkey)
                        }
                    } label: {
                        HStack(spacing: 12) {
                            InitialsAvatar(
                                name: resolved.name, identity: entry.request.fromPubkey.base64EncodedString(),
                                size: 40, photo: .person(entry.request.fromPubkey))
                            Text(resolved.listLabel).foregroundStyle(Color.lakkoText)
                            Spacer()
                            LkIconView(isOn ? .checkmarkCircleFill : .circle, size: 20)
                                .foregroundStyle(isOn ? Color.lakkoAccent : Color.lakkoTextMuted)
                                .accessibilityHidden(true)
                        }
                    }
                    .accessibilityAddTraits(isOn ? .isSelected : [])
                }
            }
        }
        .listStyle(.insetGrouped)
        .disabled(sending)
        .navigationTitle(l10n.t(publisher ? "channels.publishers.add" : "channels.info.invite_people"))
        .navigationBarTitleDisplayMode(.inline)
        .task { await loadInChannel() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(selected.isEmpty || publisher
                    ? l10n.t("channels.info.invite")
                    : l10n.t("channels.info.invite") + " (\(selected.count))") {
                    Task { await invite() }
                }
                .disabled(selected.isEmpty || sending)
            }
        }
    }

    private func invite() async {
        sending = true
        defer { sending = false }
        var failed: [String] = []
        for entry in candidates where selected.contains(entry.request.fromPubkey) {
            var oneToOne = contacts.chatGroupIdHex(for: entry)
            if oneToOne == nil { oneToOne = await contacts.openOrStartChat(for: entry) }
            guard let groupIdHex = oneToOne else {
                failed.append(entry.displayName ?? "")
                continue
            }
            do {
                let invitation = publisher
                    ? try await publisherInvitation()
                    : try await ChannelInvites.invitation(
                        channelIdHex: channelId, state: state, client: client, currentSeed: accountIdentitySeed)
                try await chat.sendMessage(
                    groupIdHex: groupIdHex, plaintext: invitation.url, channelInviteCard: invitation.card)
            } catch {
                failed.append(entry.displayName ?? "")
            }
        }
        onDone(failed)
    }

    /// A one-use publisher link for the person picked (UX 15.2).
    private func publisherInvitation() async throws -> (url: String, card: Data) {
        let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
        guard let group = try CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId) else {
            throw ChannelInvites.InviteError.notPublisher
        }
        let name = state.channels.first { $0.id == channelId }?.title
        let minted = try await CoPublisher.mintPublisherLink(
            identity: identity, publisherGroup: group,
            channelName: (name?.isEmpty ?? true) ? nil : name,
            accountIdB64: AcceptedContactsStore.boundAccountIdB64)
        return (minted.url, minted.card)
    }
}

/// Section 14.2 — `Invite link`: the group's screen — a new link with an
/// optional label, expiry and use limit, the list with `Copy link` and
/// `Revoke`. A public channel has one too; it opens the page of section 7.2.
struct ChannelInviteLinkScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    var body: some View {
        // Section 14.2 — the group's screen, over the channel's links.
        InviteLinksListView(
            load: { ChannelInvites.links(channelIdHex: channelId).map(Self.row) },
            create: { label, expiryMs, maxUses in
                _ = try await ChannelInvites.mintLink(
                    channelIdHex: channelId, state: state, client: client, currentSeed: accountIdentitySeed,
                    label: label, expiryMs: expiryMs, maxUses: maxUses)
            },
            revoke: { row in
                guard let link = ChannelInvites.links(channelIdHex: channelId).first(where: { $0.slug == row.id })
                else { return nil }
                try await ChannelInvites.revoke(link, client: client, currentSeed: accountIdentitySeed)
                return nil
            },
            createErrorKey: Self.mintErrorKey,
            revokeErrorKey: { error in
                error is URLError || DsTransientFailure.isNetwork(error)
                    ? "group_handle.error.offline" : "channels.links.revoke_failed"
            })
    }

    static func row(_ link: ChannelInvites.MintedLink) -> InviteLinkListRow {
        InviteLinkListRow(id: link.slug, url: link.url, label: link.label)
    }

    /// The line a failed mint shows: the real cause, never a network problem
    /// that did not happen.
    static func mintErrorKey(_ error: Error) -> String {
        if case ChannelInvites.InviteError.noAdmissionSeedYet = error { return "channels.links.needs_owner" }
        if error is URLError || DsTransientFailure.isNetwork(error) { return "group_handle.error.offline" }
        return "channels.links.failed"
    }
}

/// The new link's terms: the contact link's presets (the same bounds web and
/// Android offer).
struct ChannelLinkForm: Equatable {
    var label = ""
    var expiry: InviteCreateForm.Expiry = .never
    var uses: InviteCreateForm.Uses = .unlimited

    /// The wire expiry: unix ms, `0` never.
    func expiryMs(nowMs: Int64) -> UInt64 {
        let day = InviteCreateForm.dayMs
        switch expiry {
        case .oneDay: return UInt64(nowMs + day)
        case .sevenDays: return UInt64(nowMs + 7 * day)
        case .thirtyDays: return UInt64(nowMs + 30 * day)
        case .never, .custom: return 0
        }
    }

    /// The wire use limit: `0` one use, `UInt32.max` unlimited.
    var maxUses: UInt32 {
        switch uses {
        case .one: return InviteCreateForm.singleUseWire
        case .upToTen: return 10
        case .unlimited, .custom: return InviteCreateForm.unlimitedUsesWire
        }
    }
}
