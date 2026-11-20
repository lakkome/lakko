// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// Base scenario 29 (2026-11-13): the Chats row of an older message whose text
/// is a bare `/g/` link (no card stored) showed the link with its secret. The
/// rule is the web's (`channel-invite-preview.ts`): the link without its
/// secret while it is not opened, the invalid-link sentence when it is gone,
/// the invitation when it is a channel's.
@MainActor
final class BareGroupLinkTests: XCTestCase {
    private let origin = "https://test.lakko.me"
    private let link = "https://test.lakko.me/g/AbCdEfGh#0123456789abcdef"

    func testOnlyAWholeGroupLinkOfThisDeploymentIsOne() {
        XCTAssertEqual(BareGroupLink.link(of: " \(link)\n", ownOrigin: origin), link)
        XCTAssertNil(BareGroupLink.link(of: "see \(link)", ownOrigin: origin))
        XCTAssertNil(BareGroupLink.link(of: "https://other.example/g/AbCdEfGh#s", ownOrigin: origin))
        XCTAssertNil(BareGroupLink.link(of: "https://test.lakko.me/g/AbCdEfGh", ownOrigin: origin))
        XCTAssertNil(BareGroupLink.link(of: "https://test.lakko.me/c/AbCdEfGh#s", ownOrigin: origin))
    }

    func testTheRowNeverShowsTheSecret() {
        let bare = "https://test.lakko.me/g/AbCdEfGh"
        XCTAssertEqual(BareGroupLink.rowLine(text: link, opening: nil, ownOrigin: origin), bare)
        XCTAssertEqual(BareGroupLink.rowLine(text: link, opening: .offline, ownOrigin: origin), bare)
        XCTAssertEqual(BareGroupLink.rowLine(text: link, opening: .notChannel, ownOrigin: origin), bare)
        XCTAssertEqual(
            BareGroupLink.rowLine(text: link, opening: .dead, ownOrigin: origin),
            Localization.text("invite.link.invalid"))
        let facts = ChannelLinkFacts(
            channelIdHex: String(repeating: "ab", count: 16), name: "News", description: "",
            access: .public(readKey: Data(repeating: 1, count: 32)), avatar: nil)
        XCTAssertEqual(
            BareGroupLink.rowLine(text: link, opening: .channel(facts), ownOrigin: origin),
            ChatListFormatting.channelInvitePreview(channelName: "News", publisher: false))
        XCTAssertNil(BareGroupLink.rowLine(text: "hello", opening: nil, ownOrigin: origin))
    }

    func testTheBubbleShowsTheLinkOnlyOnceItOpenedToAGroup() {
        let bare = "https://test.lakko.me/g/AbCdEfGh"
        XCTAssertEqual(BareGroupLink.bubbleWords(link: link, opening: nil), bare)
        XCTAssertEqual(BareGroupLink.bubbleWords(link: link, opening: .offline), bare)
        XCTAssertEqual(BareGroupLink.bubbleWords(link: link, opening: .dead), Localization.text("invite.link.invalid"))
        XCTAssertNil(BareGroupLink.bubbleWords(link: link, opening: .notChannel))
    }

    func testAGoneLinkIsDeadAndNoAnswerIsOffline() async {
        let links = BareGroupLinks()
        links.fetchSealed = { _ in nil }
        var outcome = await links.open(link)
        XCTAssertEqual(outcome, .dead)
        links.fetchSealed = { _ in throw GroupInviteLinkError.notFound }
        outcome = await links.open(link)
        XCTAssertEqual(outcome, .dead)
        links.fetchSealed = { _ in throw URLError(.notConnectedToInternet) }
        outcome = await links.open(link)
        XCTAssertEqual(outcome, .offline)
    }

    // MARK: - Snapshots (a reply's quote, the pinned bar, a saved message)

    /// Follow-up to scenario 29: a reply quote, a pin and a saved snapshot of
    /// a bare `/g/` link message take the row's line, never the secret.
    func testSnapshotsOfABareLinkNeverCarryTheSecret() {
        let ownLink = "\(ApiEndpoints.webOrigin)/g/QwErTyUi#0123456789abcdef"
        let message = ChatMessage(
            messageId: 7, direction: .incoming, text: ownLink, attachments: [],
            createdAt: "2026-11-13T10:00:00Z", authorKeyB64: "them")
        let links = BareGroupLinks.shared
        links.fetchSealed = { _ in nil }
        XCTAssertEqual(message.snapshotInvitationLine(conversationIdHex: nil), BareGroupLink.withoutSecret(ownLink))
        let reply = ReplySnapshot.replySnapshot(
            for: message, authorLabel: "Anna",
            invitationLine: message.snapshotInvitationLine(conversationIdHex: nil))
        XCTAssertEqual(reply.excerpt, BareGroupLink.withoutSecret(ownLink))
        links.record(.dead, for: ownLink)
        let saved = SavedMessages.snapshot(
            text: message.text, attachments: [], author: "",
            invitationLine: message.snapshotInvitationLine(conversationIdHex: nil))
        XCTAssertEqual(saved.excerpt, Localization.text("invite.link.invalid"))
        let facts = ChannelLinkFacts(
            channelIdHex: String(repeating: "cd", count: 16), name: "Walk", description: "",
            access: .public(readKey: Data(repeating: 1, count: 32)), avatar: nil)
        links.record(.channel(facts), for: ownLink)
        XCTAssertEqual(
            message.snapshotInvitationLine(conversationIdHex: nil),
            ChatListFormatting.channelInvitePreview(channelName: "Walk", publisher: false))
        // Ordinary text keeps its own text.
        let plain = ChatMessage(
            messageId: 8, direction: .incoming, text: "hello", attachments: [],
            createdAt: "2026-11-13T10:00:00Z", authorKeyB64: "them")
        XCTAssertNil(plain.snapshotInvitationLine(conversationIdHex: nil))
    }

    func testNoAnswerIsAskedAgainWithABoundedWait() {
        XCTAssertEqual((0...5).map { BareGroupLinks.retryDelayMs(attempt: $0) }, [5_000, 10_000, 20_000, 40_000, 60_000, 60_000])
    }

    /// Seen live 2026-11-13: an old publisher link carrying no channel name
    /// read "Invitation to publish in " while the device held the channel.
    /// The name this device holds stands in, then "Channel".
    func testAnInvitationWithNoNameReadsByTheHeldName() {
        let id = String(repeating: "ef", count: 16)
        XCTAssertEqual(HeldChannelNames.title(carried: " Walk ", channelIdHex: id, held: { _ in "Other" }), "Walk")
        XCTAssertEqual(HeldChannelNames.title(carried: "", channelIdHex: id, held: { _ in "Walk 22 channel" }), "Walk 22 channel")
        XCTAssertEqual(
            HeldChannelNames.title(carried: nil, channelIdHex: id, held: { _ in nil }),
            Localization.text("channels.fallback_name"))
        HeldChannelNames.shared.set([ChannelHandle(id: id, localName: "Walk 22 channel", localDescription: "", addedAtMs: 1)])
        defer { HeldChannelNames.shared.set([]) }
        let publisher = CoPublisherCardFacts(
            channelIdHex: id, channelName: "", inviterName: nil,
            link: URL(string: link)!, avatar: nil)
        XCTAssertEqual(
            BareGroupLink.rowLine(text: link, opening: .publisher(publisher), ownOrigin: origin),
            ChatListFormatting.channelInvitePreview(channelName: "Walk 22 channel", publisher: true))
    }
}
