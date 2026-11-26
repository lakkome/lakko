// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
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
    private var cache: [String: [String: Entry]] = [:]

    /// One stored card and the order it was stored in, so the oldest goes
    /// first when the store is full.
    private struct Entry: Codable {
        let card: Data
        let seq: UInt64
    }

    private static func key(_ accountIdB64: String) -> String { "lakko.channel-invite-cards.v3.\(accountIdB64)" }

    private static func slot(_ conversationIdHex: String, _ messageId: Int64) -> String {
        "\(conversationIdHex.lowercased()):\(messageId)"
    }

    /// The most cards kept per account (chosen); the oldest go first.
    static let cap = 500

    func put(card: Data, conversationIdHex: String, messageId: Int64, accountIdB64: String?) {
        guard let accountIdB64, !conversationIdHex.isEmpty else { return }
        lock.lock()
        var map = loadLocked(accountIdB64)
        let next = (map.values.map(\.seq).max() ?? 0) &+ 1
        map[Self.slot(conversationIdHex, messageId)] = Entry(card: card, seq: next)
        Self.trim(&map)
        cache[accountIdB64] = map
        lock.unlock()
        persist(map, accountIdB64: accountIdB64)
    }

    func card(conversationIdHex: String, messageId: Int64, accountIdB64: String?) -> Data? {
        guard let accountIdB64, !conversationIdHex.isEmpty else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return loadLocked(accountIdB64)[Self.slot(conversationIdHex, messageId)]?.card
    }

    /// A sent row got its DS id: the card moves from the local id to it, and
    /// the local id's entry goes.
    func move(conversationIdHex: String, from localId: Int64, to messageId: Int64, accountIdB64: String?) {
        guard let accountIdB64, !conversationIdHex.isEmpty, localId != messageId else { return }
        lock.lock()
        var map = loadLocked(accountIdB64)
        guard let entry = map.removeValue(forKey: Self.slot(conversationIdHex, localId)) else {
            lock.unlock()
            return
        }
        map[Self.slot(conversationIdHex, messageId)] = entry
        cache[accountIdB64] = map
        lock.unlock()
        persist(map, accountIdB64: accountIdB64)
    }

    /// Drop the oldest entries until the map is within ``cap``.
    private static func trim(_ map: inout [String: Entry]) {
        guard map.count > cap else { return }
        let oldest = map.sorted { $0.value.seq < $1.value.seq }.prefix(map.count - cap).map(\.key)
        for key in oldest { map.removeValue(forKey: key) }
    }

    private func persist(_ map: [String: Entry], accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    private func loadLocked(_ accountIdB64: String) -> [String: Entry] {
        if let held = cache[accountIdB64] { return held }
        let map = UserDefaults.standard.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Entry].self, from: $0) } ?? [:]
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

extension ChatMessage {
    /// ADR-0087 sections 14 and 15.2 — the line that stands for a message
    /// carrying a channel invitation card wherever its text would appear
    /// (the Chats row, a reply's quote, a pin, a saved snapshot): "Invitation
    /// to {channel}" / "Invitation to publish in {channel}". `nil` for any
    /// other message. The text of a card message is the `/g/` link with its
    /// secret, which no surface shows.
    func invitationLine(conversationIdHex: String?) -> String? {
        if let channel = channelInvite(conversationIdHex: conversationIdHex) {
            return ChatListFormatting.channelInvitePreview(channelName: channel.displayName, publisher: false)
        }
        if let publisher = coPublisherInvite(conversationIdHex: conversationIdHex) {
            return ChatListFormatting.channelInvitePreview(channelName: publisher.displayName, publisher: true)
        }
        return nil
    }
}

/// What Copy takes from a message's bubble: "the text I see"
/// (`docs/ux/message-actions.md` section 3.2). A bubble drawn as a channel
/// invitation card shows no text, so there is nothing to copy (its text is
/// the `/g/` link with its secret).
enum InvitationCopy {
    static func copyText(_ text: String?, cardDrawn: Bool) -> String? {
        guard !cardDrawn, let text, !text.isEmpty else { return nil }
        return text
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

    /// The channel's name, "Channel" when the invitation names none.
    var displayName: String {
        HeldChannelNames.title(carried: channelName, channelIdHex: channelIdHex)
    }

    /// UX 15.2 — "{name} invites you to be a publisher of {channel}." when
    /// the invitation names who sent it, else the sentence without a name.
    /// The card in a chat and the link page say the same.
    var inviteLine: String {
        let inviter = inviterName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return inviter.isEmpty
            ? Localization.text("channels.publishers.invite_page", texts: ["channel": displayName])
            : Localization.text(
                "channels.publishers.invite_page_from", texts: ["name": inviter, "channel": displayName])
    }

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

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            InitialsAvatar(
                name: facts.displayName, identity: facts.channelIdHex, size: 40,
                placeholderIcon: .megaphoneFill, photo: facts.avatar.map(AvatarPhoto.avatar))
            VStack(alignment: .leading, spacing: 4) {
                Text(facts.inviteLine)
                    .font(.body)
                    .lineLimit(3)
                if !outgoing {
                    CoPublisherInviteAction(facts: facts)
                }
            }
        }
        .frame(minWidth: 220, alignment: .leading)
        .accessibilityElement(children: .contain)
    }
}

/// Section 15.2 — the publisher invitation's control, the same on the card
/// in a chat and on the link page: `Accept` runs the join in place, its
/// progress and failure show here, `Open` once this device publishes.
struct CoPublisherInviteAction: View {
    let facts: CoPublisherCardFacts
    @ObservedObject private var l10n = Localization.shared
    @ObservedObject private var acceptance = CoPublisherAcceptance.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            action
        }
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
        // The issuer of the capability the card's link opened to (verified
        // here as it is on the link page): the owner-return target (D1).
        let issuer = try? GroupInviteCapability.decodeAndVerify(wire: opened.capabilityWire).issuerSignaturePubkey()
        return ChannelLinkFacts(
            channelIdHex: AttachmentBridge.hex(channelId), name: opened.preview.groupName ?? "",
            description: subscriber.description, access: subscriber.access,
            avatar: ProfileAvatar(opened.preview.avatar),
            inviterAccountKey: issuer)
    }
}

/// What a channel invitation card does when pressed, set by the signed-in
/// shell: open the channel subscribing, or open it when this device holds it.
@MainActor
final class ChannelInviteActions {
    static let shared = ChannelInviteActions()
    var isHeld: (String) -> Bool = { _ in false }

    /// Section 14.1 / 7.2 — whether this device holds the channel: listed,
    /// and subscribed (a subscriber key) or publishing (publisher keys).
    /// The card and the link page then read `Open`.
    static func holds(_ state: ChannelsUiState, channelId: String) -> Bool {
        state.channels.contains { $0.id == channelId }
            && (state.subscriberKeysByChannel[channelId] != nil || state.keysByChannel[channelId] != nil)
    }
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
                name: facts.displayName, identity: facts.channelIdHex, size: 40,
                placeholderIcon: .megaphoneFill, photo: facts.avatar.map(AvatarPhoto.avatar))
            VStack(alignment: .leading, spacing: 4) {
                Text(facts.displayName)
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

extension ChannelLinkFacts {
    /// The channel's name as an invitation shows it (``HeldChannelNames``).
    var displayName: String { HeldChannelNames.title(carried: name, channelIdHex: channelIdHex) }
}

/// The names of the channels this device holds, for an invitation that
/// carries none (an older `/g/` link): web `cardTitle`. Seen live 2026-11-13,
/// a row read "Invitation to publish in " with no name while the iPhone was
/// subscribed to the channel. Kept current by the signed-in shell; read from
/// any thread.
final class HeldChannelNames: @unchecked Sendable {
    static let shared = HeldChannelNames()
    private let lock = NSLock()
    private var names: [String: String] = [:]

    /// Replace the held names with `channels`' titles (shared name, local
    /// name, handle).
    func set(_ channels: [ChannelHandle]) {
        var next: [String: String] = [:]
        for c in channels { next[c.id.lowercased()] = c.title }
        lock.lock()
        names = next
        lock.unlock()
    }

    private var following: AnyCancellable?

    /// Keep the held names current from the channel store (the latest value
    /// first, then every change); a new store replaces the last one.
    func follow(_ channels: AnyPublisher<[ChannelHandle], Never>) {
        let sink = channels.sink { [weak self] in self?.set($0) }
        lock.lock()
        following = sink
        lock.unlock()
    }

    func name(channelIdHex: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return names[channelIdHex.lowercased()]
    }

    /// The name the invitation carries, else the name this device holds for
    /// the channel, else "Channel".
    static func title(
        carried: String?, channelIdHex: String,
        held: (String) -> String? = { HeldChannelNames.shared.name(channelIdHex: $0) }
    ) -> String {
        let name = carried?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !name.isEmpty { return name }
        if let known = held(channelIdHex)?.trimmingCharacters(in: .whitespacesAndNewlines), !known.isEmpty {
            return known
        }
        return Localization.text("channels.fallback_name")
    }
}
