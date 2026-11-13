// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// A message whose whole text is one `/g/` link and that carries no
/// invitation card (ADR-0087 section 14; an older message, sent before cards
/// existed, or a link pasted as text). The twin of the web's
/// `apps/web/src/chat/channel-invite-preview.ts` and `group-link.ts`.
///
/// Seen live 2026-11-13: the Chats row of such a message showed the link with
/// its secret. The rule: the link is opened through the ledger once per run;
/// until that finishes, and when the ledger does not answer or the link opens
/// to a group, the row says the link WITHOUT what follows `#`; a link that is
/// gone says `invite.link.invalid`; a channel's says the invitation. The
/// bubble draws the card for a channel's link, the same words as the row
/// while the link is not opened or is gone, and the link itself only once it
/// opened to a group (a live link, tapped to join).
enum BareGroupLink {
    /// What opening a link through the ledger came to.
    enum Opening: Equatable {
        /// A channel's subscriber invitation.
        case channel(ChannelLinkFacts)
        /// An invitation to publish in a channel.
        case publisher(CoPublisherCardFacts)
        /// The link opened and is a group's: a live link.
        case notChannel
        /// The ledger has no row for it (used up, revoked, expired) or the
        /// link is damaged: nothing it says can be shown.
        case dead
        /// The ledger did not answer.
        case offline
    }

    private static let token: Set<Character> = Set(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-")
    private static let slugMax = 512
    private static let secretMax = 128

    /// The link `text` is, when it is exactly one `/g/<slug>#<secret>` link on
    /// `ownOrigin` (surrounding white space aside, no query, no user info),
    /// else `nil`. Taken apart by hand, as ``CallInviteLink`` does.
    static func link(of text: String, ownOrigin: String = ApiEndpoints.webOrigin) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed.contains(where: { $0.isWhitespace }) { return nil }
        guard let own = CallInviteLink.origin(of: ownOrigin),
              let schemeEnd = trimmed.range(of: "://") else { return nil }
        let afterScheme = trimmed[schemeEnd.upperBound...]
        guard let pathStart = afterScheme.firstIndex(of: "/") else { return nil }
        let authority = afterScheme[..<pathStart]
        if authority.contains("@") { return nil }
        let linkOrigin = (String(trimmed[..<schemeEnd.lowerBound]) + "://" + authority).lowercased()
        guard linkOrigin == own else { return nil }
        let rest = afterScheme[pathStart...]
        guard let hash = rest.firstIndex(of: "#") else { return nil }
        let path = rest[..<hash]
        let secret = rest[rest.index(after: hash)...]
        guard path.hasPrefix(GroupInviteUrl.urlPrefix) else { return nil }
        let slug = path.dropFirst(GroupInviteUrl.urlPrefix.count)
        guard isToken(slug, max: slugMax), isToken(secret, max: secretMax) else { return nil }
        return trimmed
    }

    /// `link` without its secret: what follows `#` is never drawn.
    static func withoutSecret(_ link: String) -> String {
        guard let hash = link.firstIndex(of: "#") else { return link }
        return String(link[..<hash])
    }

    /// The Chats row's (and a notification's) line for a message text, or
    /// `nil` when the text is not a bare `/g/` link. Pure: the opening is the
    /// caller's (``BareGroupLinks/opening(_:)``).
    static func rowLine(text: String, opening: Opening?, ownOrigin: String = ApiEndpoints.webOrigin) -> String? {
        guard let link = link(of: text, ownOrigin: ownOrigin) else { return nil }
        switch opening {
        case .channel(let facts)?:
            return ChatListFormatting.channelInvitePreview(channelName: facts.displayName, publisher: false)
        case .publisher(let facts)?:
            return ChatListFormatting.channelInvitePreview(channelName: facts.displayName, publisher: true)
        case .dead?:
            return groupInviteInvalidMessage
        case .notChannel?, .offline?, nil:
            return withoutSecret(link)
        }
    }

    /// The words a bubble draws in place of the link, or `nil` when the
    /// bubble draws something else: the card (a channel's link) or the link
    /// itself (a live group link). Pure.
    static func bubbleWords(link: String, opening: Opening?) -> String? {
        switch opening {
        case nil, .offline?: return withoutSecret(link)
        case .dead?: return groupInviteInvalidMessage
        case .channel?, .publisher?, .notChannel?: return nil
        }
    }

    private static func isToken<S: StringProtocol>(_ value: S, max: Int) -> Bool {
        !value.isEmpty && value.count <= max && value.allSatisfy { token.contains($0) }
    }
}

/// The ledger openings of bare `/g/` links, once per link per run, shared by
/// the Chats rows and the bubbles so a redraw never asks again.
@MainActor
final class BareGroupLinks: ObservableObject {
    static let shared = BareGroupLinks()

    /// Fetch the sealed link from the ledger by its slug; `nil` when the
    /// ledger has no row. Replaced in tests.
    var fetchSealed: (String) async throws -> (sealed: Data, blobId: Data)? = { slug in
        guard case .pointer(let blobId) = try? GroupInviteUrl.parseGroupInviteSlug(slug) else {
            throw GroupInviteLinkError.malformed
        }
        guard let sealed = try await DiscoveryClient().fetchGroupInvite(blobId: blobId) else { return nil }
        return (sealed, blobId)
    }

    @Published private(set) var openings: [String: BareGroupLink.Opening] = [:]
    private var started: Set<String> = []

    /// What `link`'s opening came to, or `nil` while it has not finished
    /// (the call starts it).
    func opening(_ link: String) -> BareGroupLink.Opening? {
        if let held = openings[link] { return held }
        start(link)
        return nil
    }

    /// Start opening `link`, once.
    func start(_ link: String) {
        guard !started.contains(link) else { return }
        started.insert(link)
        Task { @MainActor in
            // No answer from the ledger is shown (the link without its
            // secret) and asked again later, never kept as the answer.
            var attempt = 0
            while true {
                guard let outcome = await open(link) else {
                    // Cancelled: nothing was learned; the next draw asks again.
                    started.remove(link)
                    return
                }
                record(outcome, for: link)
                guard outcome == .offline else { return }
                try? await Task.sleep(nanoseconds: Self.retryDelayMs(attempt: attempt) * 1_000_000)
                attempt += 1
            }
        }
    }

    /// How long to wait before asking the ledger again after attempt
    /// `attempt` (0-based) did not reach it: 5 s, doubling, at most 60 s
    /// (chosen; the same on Android).
    nonisolated static func retryDelayMs(attempt: Int) -> UInt64 {
        min(5_000 << UInt64(min(attempt, 4)), 60_000)
    }

    /// File what opening `link` came to.
    func record(_ outcome: BareGroupLink.Opening, for link: String) {
        started.insert(link)
        openings[link] = outcome
    }

    /// Open `link` through the ledger. `nil` when the work was cancelled.
    func open(_ link: String) async -> BareGroupLink.Opening? {
        let split = InviteLinkSecret.split(link)
        guard let secret = InviteLinkSecret.read(split.fragment),
              let slugStart = split.base.range(of: GroupInviteUrl.urlPrefix) else { return .dead }
        let slug = String(split.base[slugStart.upperBound...])
        let located: (sealed: Data, blobId: Data)
        do {
            guard let fetched = try await fetchSealed(slug) else { return .dead }
            located = fetched
        } catch is GroupInviteLinkError {
            return .dead
        } catch {
            if Cancellation.endedByCancellation(error) { return nil }
            return .offline
        }
        guard let cap = try? GroupInviteCapability.openLink(sealed: located.sealed, linkSecret: secret),
              cap.isValidNow(nowMs: UInt64(Date().timeIntervalSince1970 * 1000)) else { return .dead }
        switch JoinGroupLandingViewModel.previewOutcome(of: cap, secret: secret) {
        case .channel(_, let facts):
            return .channel(facts)
        case .publisher(_, let invite):
            guard let url = URL(string: link) else { return .dead }
            return .publisher(CoPublisherCardFacts(
                channelIdHex: invite.channelIdHex, channelName: invite.channelName,
                inviterName: invite.inviterName, link: url, avatar: invite.avatar))
        case .none, .opened:
            return .notChannel
        case .damaged:
            return .dead
        }
    }
}

/// A bubble whose text is a bare `/g/` link (``BareGroupLink``): the card
/// once the link opens to a channel's invitation, the link once it opens to
/// a group's, and otherwise the link without its secret or the invalid-link
/// sentence.
struct BareGroupLinkBubble: View {
    let link: String
    let outgoing: Bool
    let linkColor: Color
    let onOwnLink: (URL) -> Void
    @ObservedObject private var links = BareGroupLinks.shared

    var body: some View {
        let opening = links.openings[link]
        Group {
            switch opening {
            case .channel(let facts)?:
                ChannelInvitationCard(facts: facts, outgoing: outgoing)
            case .publisher(let facts)?:
                CoPublisherInvitationCard(facts: facts, outgoing: outgoing)
            case .notChannel?:
                MessageText(text: link, linkColor: linkColor, onOwnLink: onOwnLink)
                    .chatFont(.body)
            case .dead?, .offline?, nil:
                Text(BareGroupLink.bubbleWords(link: link, opening: opening) ?? BareGroupLink.withoutSecret(link))
                    .chatFont(.body)
            }
        }
        .onAppear { links.start(link) }
    }
}

extension ChatMessage {
    /// What stands for this message's text in a snapshot taken of it (a
    /// reply's quote, the pinned bar, a saved message): the invitation line
    /// of a card it carries, else, for a bare `/g/` link, the row's line
    /// (``BareGroupLink/rowLine(text:opening:ownOrigin:)``), never the
    /// link's secret. `nil` keeps the message's own text. The web's
    /// `rowInvitationLine`.
    @MainActor
    func snapshotInvitationLine(conversationIdHex: String?) -> String? {
        if let line = invitationLine(conversationIdHex: conversationIdHex) { return line }
        guard !deleted, let link = BareGroupLink.link(of: text) else { return nil }
        return BareGroupLink.rowLine(text: text, opening: BareGroupLinks.shared.opening(link))
    }
}
