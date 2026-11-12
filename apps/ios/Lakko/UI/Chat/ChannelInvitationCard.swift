// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// ADR-0087 section 14 — the channel invitation card a message carries
/// (`channel_invite` in the chat envelope), kept per account beside the
/// message log and keyed on (conversation, DS message id), like the other
/// per-message state. Written when a card arrives (at receipt, with the row's
/// id) or is sent (under the row's local id, moved to its DS id once
/// published); read when the bubble is drawn.
final class ChannelInviteCardStore: @unchecked Sendable {
    static let shared = ChannelInviteCardStore()
    private let lock = NSLock()
    private var cache: [String: [String: Data]] = [:]

    private static func key(_ accountIdB64: String) -> String { "lakko.channel-invite-cards.v2.\(accountIdB64)" }

    private static func slot(_ conversationIdHex: String, _ messageId: Int64) -> String {
        "\(conversationIdHex.lowercased()):\(messageId)"
    }

    /// The most cards kept per account (chosen); the oldest go first.
    static let cap = 500

    func put(card: Data, conversationIdHex: String, messageId: Int64, accountIdB64: String?) {
        guard let accountIdB64, !conversationIdHex.isEmpty else { return }
        lock.lock()
        var map = loadLocked(accountIdB64)
        map[Self.slot(conversationIdHex, messageId)] = card
        if map.count > Self.cap { map.remove(at: map.startIndex) }
        cache[accountIdB64] = map
        lock.unlock()
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    func card(conversationIdHex: String, messageId: Int64, accountIdB64: String?) -> Data? {
        guard let accountIdB64, !conversationIdHex.isEmpty else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return loadLocked(accountIdB64)[Self.slot(conversationIdHex, messageId)]
    }

    /// A sent row got its DS id: the card moves from the local id to it.
    func move(conversationIdHex: String, from localId: Int64, to messageId: Int64, accountIdB64: String?) {
        guard let card = card(conversationIdHex: conversationIdHex, messageId: localId, accountIdB64: accountIdB64)
        else { return }
        put(card: card, conversationIdHex: conversationIdHex, messageId: messageId, accountIdB64: accountIdB64)
    }

    private func loadLocked(_ accountIdB64: String) -> [String: Data] {
        if let held = cache[accountIdB64] { return held }
        let map = UserDefaults.standard.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Data].self, from: $0) } ?? [:]
        cache[accountIdB64] = map
        return map
    }

    /// Forget device: drop the account's cards.
    func delete(accountIdB64: String) {
        lock.lock()
        cache[accountIdB64] = nil
        lock.unlock()
        UserDefaults.standard.removeObject(forKey: Self.key(accountIdB64))
    }
}

extension ChatMessage {
    /// ADR-0087 section 14 — the channel invitation this message of
    /// `conversationIdHex` carries, opened from its card. `nil` for any other
    /// message.
    func channelInvite(conversationIdHex: String?) -> ChannelLinkFacts? {
        guard !deleted, let conversationIdHex else { return nil }
        let account = AcceptedContactsStore.boundAccountIdB64
        // The DS id once published; the local id while sending.
        let ids = [messageId, localId].filter { $0 != 0 }
        guard let card = ids.lazy.compactMap({
            ChannelInviteCardStore.shared.card(conversationIdHex: conversationIdHex, messageId: $0, accountIdB64: account)
        }).first else { return nil }
        return ChannelInviteCardFacts.facts(card: card)
    }
}

extension ChatMessage {
    /// ADR-0087 section 15.2 (phase B) — a co-publisher invitation (card
    /// kind 2) this message carries. `nil` for any other message.
    func coPublisherInvite(conversationIdHex: String?) -> CoPublisherCardFacts? {
        guard !deleted, let conversationIdHex else { return nil }
        let account = AcceptedContactsStore.boundAccountIdB64
        let ids = [messageId, localId].filter { $0 != 0 }
        guard let card = ids.lazy.compactMap({
            ChannelInviteCardStore.shared.card(conversationIdHex: conversationIdHex, messageId: $0, accountIdB64: account)
        }).first else { return nil }
        return CoPublisherCardFacts.facts(card: card)
    }
}

/// What a co-publisher invitation card says: the channel, who invites, and
/// the link that answers it.
struct CoPublisherCardFacts: Equatable {
    let channelIdHex: String
    let channelName: String
    let inviterName: String?
    let link: URL
    /// The channel's photo the link's preview carries.
    var avatar: ProfileAvatar? = nil

    static func facts(card: Data) -> CoPublisherCardFacts? {
        guard let opened = try? openChannelInviteCard(card: card),
              opened.preview.subscriber == nil,
              let channelId = opened.preview.channelId, channelId.count == 16,
              let text = ChannelInviteText.link(card: card), let url = URL(string: text)
        else { return nil }
        return CoPublisherCardFacts(
            channelIdHex: AttachmentBridge.hex(channelId), channelName: opened.preview.groupName ?? "",
            inviterName: opened.preview.inviterName, link: url, avatar: ProfileAvatar(opened.preview.avatar))
    }
}

/// UX 15.2 — the card in the one-to-one: "{name} invites you to be a
/// publisher of {channel}." with `Accept`, which runs the whole join in place (no second page).
struct CoPublisherInvitationCard: View {
    let facts: CoPublisherCardFacts
    let outgoing: Bool
    @ObservedObject private var l10n = Localization.shared
    /// Section 15.2 — the join runs from the card; its progress and failure
    /// show here.
    @ObservedObject private var acceptance = CoPublisherAcceptance.shared

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            InitialsAvatar(
                name: facts.channelName, identity: facts.channelIdHex, size: 40,
                placeholderIcon: .megaphoneFill, photo: facts.avatar.map(AvatarPhoto.avatar))
            VStack(alignment: .leading, spacing: 4) {
                Text(l10n.t("channels.publishers.invite_page", texts: ["channel": facts.channelName]))
                    .font(.body)
                    .lineLimit(3)
                if !outgoing {
                    action
                }
            }
        }
        .frame(minWidth: 220, alignment: .leading)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var action: some View {
        // UX 15.2.2 — the waiting states; a refusal is never told.
        switch acceptance.cardState(facts) {
        case .open:
            pill(l10n.t("search.conversation.open")) { acceptance.openChannel(facts.channelIdHex) }
        case .accept:
            pill(l10n.t("channels.publishers.accept")) { acceptance.accept(facts) }
        case .sending:
            HStack(spacing: 8) {
                ProgressView()
                waitingLine
            }
            .padding(.vertical, 5)
        case .waiting:
            waitingLine
                .padding(.vertical, 5)
        case .noAnswer:
            Text(l10n.t("requests_sent.no_answer"))
                .font(.footnote)
                .opacity(0.8)
            pill(l10n.t("requests_sent.ask_again")) { acceptance.accept(facts) }
        case .linkDead:
            Text(groupInviteInvalidMessage)
                .font(.footnote)
                .foregroundStyle(Color.lakkoDanger)
        case .failed(let failure, let askAgain):
            Text(failure.message.text)
                .font(.footnote)
                .foregroundStyle(Color.lakkoDanger)
            if failure.offersRetry {
                pill(askAgain ? l10n.t("requests_sent.ask_again") : joinTryAgain) { acceptance.accept(facts) }
            }
        }
    }

    private var waitingLine: some View {
        Text(l10n.t("channels.join.waiting"))
            .font(.footnote)
            .opacity(0.8)
    }

    private func pill(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lakkoAccentForeground)
                .padding(.horizontal, 14)
                .frame(minHeight: 34)
                .background(Capsule().fill(Color.lakkoAccent))
                .padding(.vertical, 5)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

enum ChannelInviteCardFacts {
    /// What a card says, opened offline from its sealed capability.
    static func facts(card: Data) -> ChannelLinkFacts? {
        guard let opened = try? openChannelInviteCard(card: card),
              let subscriber = opened.preview.subscriber,
              let channelId = opened.preview.channelId, channelId.count == 16 else { return nil }
        return ChannelLinkFacts(
            channelIdHex: AttachmentBridge.hex(channelId), name: opened.preview.groupName ?? "",
            description: subscriber.description, access: subscriber.access,
            avatar: ProfileAvatar(opened.preview.avatar))
    }
}

/// What a channel invitation card does when pressed, set by the signed-in
/// shell: open the channel subscribing, or open it when this device holds it.
@MainActor
final class ChannelInviteActions {
    static let shared = ChannelInviteActions()
    var isHeld: (String) -> Bool = { _ in false }
    var subscribe: (ChannelLinkFacts) -> Void = { _ in }
}

/// Section 14 — the card in the chat:
///
/// ```
/// [avatar]  Berlin news
///           Public channel
///           [ Subscribe ]
/// ```
///
/// `Subscribe` becomes `Open` once subscribed. Drawn inside the bubble.
struct ChannelInvitationCard: View {
    let facts: ChannelLinkFacts
    let outgoing: Bool
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            InitialsAvatar(
                name: facts.name, identity: facts.channelIdHex, size: 40,
                placeholderIcon: .megaphoneFill, photo: facts.avatar.map(AvatarPhoto.avatar))
            VStack(alignment: .leading, spacing: 4) {
                Text(facts.name)
                    .font(.body.weight(.semibold))
                    .lineLimit(2)
                Text(l10n.t(facts.isPublic ? "channels.header.public" : "channels.header.private"))
                    .font(.footnote)
                    .opacity(0.8)
                let held = ChannelInviteActions.shared.isHeld(facts.channelIdHex)
                Button {
                    ChannelInviteActions.shared.subscribe(facts)
                } label: {
                    Text(l10n.t(held ? "search.conversation.open" : "channels.subscribe.action"))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(outgoing ? Color.lakkoAccent : Color.lakkoAccentForeground)
                        .padding(.horizontal, 14)
                        .frame(minHeight: 34)
                        .background(Capsule().fill(outgoing ? Color.lakkoAccentForeground : Color.lakkoAccent))
                        .padding(.vertical, 5)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .frame(minWidth: 220, alignment: .leading)
        .accessibilityElement(children: .contain)
    }
}
