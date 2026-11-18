// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0052 decision 2 / `docs/ux/message-actions.md` sections 5 and 5.1 —
/// what is sent when a message is forwarded, what a received forward becomes,
/// and what the attribution line says.
final class ForwardPayloadTests: XCTestCase {

    private let origin = "0a0b0c0d"

    private func message(
        _ text: String,
        id: Int64 = 41,
        direction: Lakko.Direction = .incoming,
        edited: Bool = false,
        reply: ReplyRef? = nil,
        forward: ForwardRef? = nil,
        reactions: [Reaction] = []
    ) -> ChatMessage {
        ChatMessage(
            messageId: id,
            direction: direction,
            text: text,
            createdAt: "2026-09-26T10:00:00Z",
            authorKeyB64: "them",
            edited: edited,
            reactions: reactions,
            reply: reply,
            forward: forward)
    }

    // MARK: - What is sent

    func testAForwardCarriesTheTextTheAuthorsLabelAndThisMessageAsTheOrigin() {
        let sent = ForwardPayload.forwardPayload(
            of: message("see you at 5"), authorLabel: " Anna ", originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertEqual(sent.text, "see you at 5")
        XCTAssertEqual(sent.forward.author, "Anna")
        XCTAssertEqual(sent.forward.originGroupIdHex, origin)
        XCTAssertEqual(sent.forward.originMessageId, 41)
        XCTAssertTrue(sent.forward.hasOrigin)
    }

    /// Section 5.1 — "the original's text verbatim (the edited text if it was
    /// edited)". After an edit lands `text` IS the edited text.
    func testAnEditedMessageForwardsItsEditedText() {
        let sent = ForwardPayload.forwardPayload(
            of: message("corrected", edited: true), authorLabel: "Anna", originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertEqual(sent.text, "corrected")
    }

    /// Section 5.1 — the quote of a reply is NOT carried, and neither are
    /// reactions: the payload has no field for either, and the text is the
    /// message's own.
    func testAReplysQuoteAndReactionsAreNotCarried() {
        let quoted = ReplyRef(target: 7, author: "Bob", excerpt: "the plan")
        let sent = ForwardPayload.forwardPayload(
            of: message(
                "agreed", reply: quoted, reactions: [Reaction(reactor: "x", emoji: "👍")]),
            authorLabel: "Anna", originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertEqual(sent.text, "agreed")
        XCTAssertEqual(sent.forward.author, "Anna")
        XCTAssertFalse(sent.text.contains("the plan"))
    }

    /// A blank label is no name: the recipient reads "Forwarded".
    func testABlankAuthorLabelMeansNoName() {
        XCTAssertNil(
            ForwardPayload.forwardPayload(
                of: message("hi"), authorLabel: "  ", originGroupIdHex: origin, sourceIsOneToOne: false
            ).forward.author)
        XCTAssertNil(
            ForwardPayload.forwardPayload(
                of: message("hi"), authorLabel: nil, originGroupIdHex: origin, sourceIsOneToOne: false
            ).forward.author)
    }

    /// A row without a server id has nothing to point at, so no origin — and
    /// never half of one.
    func testARowWithoutAnIdCarriesNoOrigin() {
        let sent = ForwardPayload.forwardPayload(
            of: message("hi", id: 0), authorLabel: "Anna", originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertNil(sent.forward.originGroupIdHex)
        XCTAssertNil(sent.forward.originMessageId)
        XCTAssertFalse(sent.forward.hasOrigin)
    }

    // MARK: - ADR-0053 section 6: a forward out of a one-to-one

    /// A forward whose source is a one-to-one carries no origin: the only
    /// other holder of that conversation is its other participant.
    func testAForwardFromAOneToOneCarriesNoOrigin() {
        let sent = ForwardPayload.forwardPayload(
            of: message("see you at 5"), authorLabel: "Anna", originGroupIdHex: origin,
            sourceIsOneToOne: true)
        XCTAssertEqual(sent.text, "see you at 5")
        XCTAssertEqual(sent.forward.author, "Anna", "the name is still carried")
        XCTAssertNil(sent.forward.originGroupIdHex)
        XCTAssertNil(sent.forward.originMessageId)
        XCTAssertFalse(sent.forward.hasOrigin)
    }

    /// ...nor an origin the message carried in: the rule is about the source
    /// conversation, and it drops the whole pair.
    func testAForwardOfAForwardOutOfAOneToOneDropsTheCarriedOriginToo() {
        let carried = ForwardRef(author: "Zoe", originGroupIdHex: "ffee", originMessageId: 3)
        let sent = ForwardPayload.forwardPayload(
            of: message("hello", id: 90, forward: carried),
            authorLabel: "Anna",
            originGroupIdHex: origin,
            sourceIsOneToOne: true)
        XCTAssertEqual(sent.forward.author, "Zoe")
        XCTAssertFalse(sent.forward.hasOrigin)
    }

    // MARK: - Forward of a forward

    /// Section 5: "a message forwarded onward keeps the ORIGINAL author's name
    /// rather than the name of whoever forwarded it" — and the ORIGINAL
    /// origin, never this conversation and this message.
    func testAForwardOfAForwardKeepsTheOriginalNameAndOrigin() {
        let carried = ForwardRef(author: "Zoe", originGroupIdHex: "ffee", originMessageId: 3)
        let sent = ForwardPayload.forwardPayload(
            of: message("hello", id: 90, forward: carried),
            authorLabel: "Anna",
            originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertEqual(sent.text, "hello")
        XCTAssertEqual(sent.forward.author, "Zoe")
        XCTAssertEqual(sent.forward.originGroupIdHex, "ffee")
        XCTAssertEqual(sent.forward.originMessageId, 3)
    }

    /// A forward that carried no name or no origin passes on exactly that:
    /// nothing is invented to fill the gap, or the chain would rewrite its own
    /// history with the forwarder's details.
    func testAForwardOfAnAnonymousForwardStaysAnonymousAndOriginless() {
        let carried = ForwardRef(author: nil, originGroupIdHex: nil, originMessageId: nil)
        let sent = ForwardPayload.forwardPayload(
            of: message("hello", id: 90, forward: carried),
            authorLabel: "Anna",
            originGroupIdHex: origin, sourceIsOneToOne: false)
        XCTAssertNil(sent.forward.author)
        XCTAssertFalse(sent.forward.hasOrigin)
    }

    // MARK: - What arrives

    func testAReceivedForwardConvertsTheOriginGroupFromBase64ToHex() throws {
        let b64 = try XCTUnwrap(ForwardPayload.groupIdB64(fromHex: origin))
        let received = try XCTUnwrap(
            ForwardPayload.received(author: "Anna", originGroupIdB64: b64, originMessageId: 41))
        XCTAssertEqual(received.author, "Anna")
        XCTAssertEqual(received.originGroupIdHex, origin)
        XCTAssertEqual(received.originMessageId, 41)
    }

    func testAnEnvelopeWithNoForwardFieldsIsNotAForward() {
        XCTAssertNil(ForwardPayload.received(author: nil, originGroupIdB64: nil, originMessageId: nil))
        XCTAssertNil(ForwardPayload.received(author: "  ", originGroupIdB64: nil, originMessageId: nil))
    }

    /// The origin is a pair: an id that does not decode, or a missing half,
    /// drops the whole origin and keeps the name.
    func testAnUndecodableOrHalfOriginIsDroppedAsAWhole() throws {
        let broken = try XCTUnwrap(
            ForwardPayload.received(
                author: "Anna", originGroupIdB64: "not base64!", originMessageId: 41))
        XCTAssertEqual(broken.author, "Anna")
        XCTAssertFalse(broken.hasOrigin)
        let half = try XCTUnwrap(
            ForwardPayload.received(author: "Anna", originGroupIdB64: "CgsMDQ==", originMessageId: nil))
        XCTAssertFalse(half.hasOrigin)
        XCTAssertNil(half.originGroupIdHex, "half an origin was kept")
    }

    /// A forward carrying only an origin (no name) is still a forward.
    func testAnOriginAloneIsAForward() throws {
        let received = try XCTUnwrap(
            ForwardPayload.received(author: nil, originGroupIdB64: "CgsMDQ==", originMessageId: 5))
        XCTAssertNil(received.author)
        XCTAssertEqual(received.originGroupIdHex, "0a0b0c0d")
    }

    // MARK: - The group id shapes

    func testHexAndStandardBase64RoundTrip() {
        let hex = "00ff10a5"
        XCTAssertEqual(ForwardPayload.groupIdB64(fromHex: hex), "AP8QpQ==")
        XCTAssertEqual(ForwardPayload.groupIdHex(fromB64: "AP8QpQ=="), hex)
        XCTAssertNil(ForwardPayload.groupIdB64(fromHex: "abc"))
        XCTAssertNil(ForwardPayload.groupIdB64(fromHex: "zz"))
        XCTAssertNil(ForwardPayload.groupIdB64(fromHex: ""))
        XCTAssertNil(ForwardPayload.groupIdHex(fromB64: ""))
    }

    // MARK: - The line

    func testTheLineNamesTheAuthorOrSaysForwarded() {
        XCTAssertEqual(
            ForwardPayload.attributionLine(
                ForwardRef(author: "Anna\nB", originGroupIdHex: nil, originMessageId: nil)).withoutIsolates,
            "Forwarded from Anna B")
        XCTAssertEqual(
            ForwardPayload.attributionLine(
                ForwardRef(author: nil, originGroupIdHex: nil, originMessageId: nil)),
            "Forwarded")
        XCTAssertEqual(
            ForwardPayload.attributionLine(
                ForwardRef(author: "  ", originGroupIdHex: nil, originMessageId: nil)),
            "Forwarded")
    }

    /// `ForwardRef` itself refuses half an origin, whichever way it is built.
    func testForwardRefNeverHoldsHalfAnOrigin() {
        let a = ForwardRef(author: "x", originGroupIdHex: "aa", originMessageId: nil)
        let b = ForwardRef(author: "x", originGroupIdHex: nil, originMessageId: 4)
        XCTAssertNil(a.originGroupIdHex)
        XCTAssertNil(b.originMessageId)
        XCTAssertFalse(a.hasOrigin)
        XCTAssertFalse(b.hasOrigin)
    }

    // MARK: - ADR-0090 — forwarding content messages (amended 2026-11-18)

    private func contentMessage(_ content: MessageContent) throws -> ChatMessage {
        let bytes = try composeContentEnvelope(content: content, attachments: [], parts: ContentModel.emptyParts)
        return ChatMessage(
            messageId: 41, direction: .incoming, text: "", createdAt: "2026-11-18T10:00:00Z",
            authorKeyB64: "them", content: PersistedContent(envelopeB64: bytes.base64EncodedString()))
    }

    private let cardKey = Data(repeating: 0x33, count: 32)

    func testAPlainMessageALocationAndAnArticleAreForwardableAsTheSameContent() throws {
        XCTAssertTrue(ForwardPayload.isForwardable(message("hi")) { _ in false })
        XCTAssertNil(ForwardPayload.forwardedContent(of: message("hi")))
        let location = SharedLocation(latE7: 600_000_000, lonE7: 250_000_000, accuracyM: 12, fixAtMs: 1, name: "Here")
        let located = try contentMessage(.location(location: location))
        XCTAssertTrue(ForwardPayload.isForwardable(located) { _ in false })
        guard case .location(let sent)? = ForwardPayload.forwardedContent(of: located) else {
            return XCTFail("a location forwards as a location")
        }
        XCTAssertEqual(sent, location)
    }

    func testAPollAndAChecklistAreNeverForwarded() throws {
        let body = ChecklistBody(title: "T", othersAdd: false, othersMark: false, items: [ChecklistItem(id: 1, text: "a")])
        let checklist = try contentMessage(.checklist(checklist: SignedChecklist(body: body, authorSig: nil)))
        XCTAssertFalse(ForwardPayload.isForwardable(checklist) { _ in true })
        XCTAssertNil(ForwardPayload.forwardedContent(of: checklist))
        let poll = PollBody(
            id: Data(repeating: 1, count: 16), question: "Q?", options: ["a", "b"], multiple: false,
            quiz: nil, closeAtMs: nil, anonymous: nil)
        let polled = try contentMessage(.poll(poll: SignedPoll(body: poll, authorSig: nil)))
        XCTAssertFalse(ForwardPayload.isForwardable(polled) { _ in true })
    }

    func testAContactCardIsForwardedOnlyWhenItsPersonLetsItBeShared() throws {
        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: cardKey)
        let carded = try contentMessage(.contactCard(card: card))
        var asked: [Data] = []
        XCTAssertTrue(ForwardPayload.isForwardable(carded) { asked.append($0); return true })
        XCTAssertEqual(asked, [cardKey], "the check is read for the person on the card")
        XCTAssertFalse(ForwardPayload.isForwardable(carded) { _ in false })
        guard case .contactCard(let sent)? = ForwardPayload.forwardedContent(of: carded) else {
            return XCTFail("a card forwards as a card")
        }
        XCTAssertEqual(sent, card)
    }

    func testAForwardedContentEnvelopeCarriesTheForwardLine() throws {
        let parts = ContentModel.forwardParts(
            ForwardRef(author: "Anna", originGroupIdHex: "0a0b", originMessageId: 7))
        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: cardKey)
        let bytes = try composeContentEnvelope(content: .contactCard(card: card), attachments: [], parts: parts)
        let parsed = parseBodyEnvelope(bytes: bytes)
        XCTAssertEqual(parsed.forwardedFrom, "Anna")
        XCTAssertEqual(parsed.forwardOriginMessageId, 7)
        XCTAssertEqual(parsed.forwardOriginGroupB64.flatMap(ForwardPayload.groupIdHex(fromB64:)), "0a0b")
        XCTAssertNil(ContentModel.forwardParts(nil).forwardedFrom)
    }

    func testASelectionForwardsOnlyWhenEveryMessageMay() {
        let selection = MessageSelection(remaining: [1, 2])
        XCTAssertTrue(selection.forwardEnabled(allForwardable: true))
        XCTAssertFalse(selection.forwardEnabled(allForwardable: false))
    }

    // MARK: - Retry of a failed message (aligned with web and Android)

    @MainActor
    func testARetriedForwardKeepsItsForwardLine() throws {
        let forward = ForwardRef(author: "Anna", originGroupIdHex: "0a0b", originMessageId: 7)
        let plain = ChatMessage(
            messageId: 0, direction: .outgoing, text: "see you", createdAt: "", authorKeyB64: nil,
            forward: forward)
        let parsed = parseBodyEnvelope(bytes: try ChatRepository.retryEnvelope(row: plain))
        XCTAssertEqual(parsed.text, "see you")
        XCTAssertEqual(parsed.forwardedFrom, "Anna")
        XCTAssertEqual(parsed.forwardOriginMessageId, 7)

        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: cardKey)
        let stored = try XCTUnwrap(ContentModel.store(
            content: .contactCard(card: card), attachments: [], parts: ContentModel.forwardParts(forward)))
        let content = ChatMessage(
            messageId: 0, direction: .outgoing, text: "", createdAt: "", authorKeyB64: nil,
            forward: forward, content: stored)
        let resent = parseBodyEnvelope(bytes: try ChatRepository.retryEnvelope(row: content))
        XCTAssertEqual(resent.forwardedFrom, "Anna", "a retried content forward keeps its line")
        guard case .contactCard(let back)? = resent.content else { return XCTFail("still the card") }
        XCTAssertEqual(back, card)
    }

    @MainActor
    func testAnAnonymousPollIsResentOnlyWhileItsRosterIsUnchanged() throws {
        let a = Data(repeating: 2, count: 32), b = Data(repeating: 1, count: 32), c = Data(repeating: 3, count: 32)
        let hash = try pollRingHash(keys: [b, a])
        func poll(_ anonymous: PollAnonymous?) -> MessageContent {
            .poll(poll: SignedPoll(body: PollBody(
                id: Data(repeating: 1, count: 16), question: "Q?", options: ["x", "y"], multiple: false,
                quiz: nil, closeAtMs: nil, anonymous: anonymous), authorSig: nil))
        }
        let anon = poll(PollAnonymous(secret: Data(repeating: 5, count: 32), ring0Hash: hash))
        XCTAssertTrue(ContentRing.retryAllowed(content: anon, rosterKeys: [a, b, a]),
                      "the same people in any order, a repeated key counted once")
        XCTAssertFalse(ContentRing.retryAllowed(content: anon, rosterKeys: [a, b, c]))
        XCTAssertFalse(ContentRing.retryAllowed(content: anon, rosterKeys: [a]))
        XCTAssertTrue(ContentRing.retryAllowed(content: poll(nil), rosterKeys: [c]), "a named poll always may")
        XCTAssertTrue(ContentRing.retryAllowed(content: nil, rosterKeys: []), "a plain message always may")
        XCTAssertEqual(
            ChatViewModel.errorText(for: ContentSendError.pollRingChanged, fallbackKey: "chat.send.failed").text,
            Localization.text("poll.resend_refused"))
    }
}


