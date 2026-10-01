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
}
