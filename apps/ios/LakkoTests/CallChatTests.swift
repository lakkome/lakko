// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The chat of a group or link call (`docs/ux/calls.md` section 4.1.4): the
/// per-call channel over fakes, the composer's text rule (the core's), and
/// what the panel, the badge and the preview show.
@MainActor
final class CallChatTests: XCTestCase {

    // MARK: - A channel over fakes

    /// A channel whose compose stamps `peer|seq|text` and whose verify reads
    /// it back when the signer key is `goodKey`.
    @MainActor
    private final class Harness {
        var selfPeerId: String? = "me"
        var canSend = true
        var sent: [Data] = []
        var shown: [(peer: String, seq: UInt64, text: String)] = []
        let goodKey = Data(repeating: 7, count: 32)
        lazy var channel = CallChatChannel(deps: CallChatChannel.Deps(
            compose: { selfPeerId, seq, text in Data("\(selfPeerId)|\(seq)|\(text)".utf8) },
            verify: { [goodKey] from, key, bytes in
                guard key == goodKey, let text = String(data: bytes, encoding: .utf8) else { return nil }
                let parts = text.split(separator: "|", maxSplits: 2, omittingEmptySubsequences: false)
                    .map(String.init)
                guard parts.count == 3, parts[0] == from, let seq = UInt64(parts[1]) else { return nil }
                return CallChatChannel.Verified(seq: seq, text: parts[2])
            },
            sendSealed: { [weak self] plaintext in
                guard let self, self.canSend else { return false }
                self.sent.append(plaintext)
                return true
            },
            selfPeerId: { [weak self] in self?.selfPeerId },
            onMessage: { [weak self] peer, message in
                self?.shown.append((peer, message.seq, message.text))
            }
        ))

        func wire(_ from: String, _ seq: UInt64, _ text: String) -> Data {
            Data("\(from)|\(seq)|\(text)".utf8)
        }
    }

    func testAMessageFromAnIdentifiedPeerVerifiesAndShowsOnce() {
        let h = Harness()
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "hello"))
        XCTAssertEqual(h.shown.map(\.text), ["hello"])
        // A relay frame re-sent after a reconnect does not show twice.
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "hello"))
        XCTAssertEqual(h.shown.count, 1)
        // The same seq from another peer is another message.
        h.channel.peerIdentified("p2", signerKey: h.goodKey)
        h.channel.received(from: "p2", bytes: h.wire("p2", 1, "hi"))
        XCTAssertEqual(h.shown.map(\.peer), ["p1", "p2"])
    }

    func testAMessageThatDoesNotVerifyIsDroppedSilently() {
        let h = Harness()
        h.channel.peerIdentified("p1", signerKey: Data(repeating: 9, count: 32))
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "forged"))
        // A message stamped with another peer's id fails too.
        h.channel.peerIdentified("p2", signerKey: h.goodKey)
        h.channel.received(from: "p2", bytes: h.wire("p1", 2, "relabelled"))
        XCTAssertTrue(h.shown.isEmpty)
    }

    func testMessagesBeforeTheAnnouncementAreHeldThenShownInArrivalOrder() {
        let h = Harness()
        h.channel.received(from: "p1", bytes: h.wire("p1", 2, "second"))
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "first"))
        XCTAssertTrue(h.shown.isEmpty, "held until the announcement verifies")
        XCTAssertEqual(h.channel.pendingCount("p1"), 2)
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertEqual(h.shown.map(\.text), ["second", "first"], "arrival order, not seq order")
        XCTAssertEqual(h.channel.pendingCount("p1"), 0)
        // A later re-announcement shows nothing again.
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertEqual(h.shown.count, 2)
    }

    func testAtMostFiftyAreHeldPerPeerAndTheOldestGoes() {
        let h = Harness()
        for seq in 1...55 {
            h.channel.received(from: "p1", bytes: h.wire("p1", UInt64(seq), "m\(seq)"))
        }
        XCTAssertEqual(h.channel.pendingCount("p1"), CallChatChannel.maxPendingPerPeer)
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertEqual(h.shown.first?.text, "m6")
        XCTAssertEqual(h.shown.last?.text, "m55")
        XCTAssertEqual(h.shown.count, 50)
    }

    func testAPeerThatLeavesBeforeItIsNamedIsNeverShown() {
        let h = Harness()
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "never"))
        h.channel.peerLeft("p1")
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertTrue(h.shown.isEmpty)
    }

    func testOwnMessagesAreNumberedFromOne() {
        let h = Harness()
        XCTAssertEqual(h.channel.send("a").seq, 1)
        XCTAssertEqual(h.channel.send("b").seq, 2)
        XCTAssertEqual(h.channel.send("c").seq, 3)
        XCTAssertEqual(h.sent.map { String(decoding: $0, as: UTF8.self) }, ["me|1|a", "me|2|b", "me|3|c"])
    }

    func testAFailedSendIsNotSentAndSendAgainSendsTheSameBytes() {
        let h = Harness()
        h.canSend = false
        let first = h.channel.send("hello")
        XCTAssertEqual(first.seq, 1)
        XCTAssertFalse(first.sent)
        XCTAssertFalse(h.channel.resend(seq: 1), "still no socket")
        h.canSend = true
        // A later message takes the next number; Send again keeps its own.
        XCTAssertEqual(h.channel.send("later").seq, 2)
        XCTAssertTrue(h.channel.resend(seq: 1))
        XCTAssertEqual(
            h.sent.map { String(decoding: $0, as: UTF8.self) }, ["me|2|later", "me|1|hello"])
        XCTAssertFalse(h.channel.resend(seq: 1), "a sent message is not sent again")
    }

    func testASendBeforeTheWelcomeIsComposedOnSendAgainUnderTheSameNumber() {
        let h = Harness()
        h.selfPeerId = nil
        XCTAssertFalse(h.channel.send("early").sent)
        h.selfPeerId = "me"
        XCTAssertTrue(h.channel.resend(seq: 1))
        XCTAssertEqual(h.sent.map { String(decoding: $0, as: UTF8.self) }, ["me|1|early"])
    }

    func testTheCallsEndClearsEverything() {
        let h = Harness()
        h.canSend = false
        _ = h.channel.send("unsent")
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "held"))
        h.channel.clear()
        h.canSend = true
        XCTAssertFalse(h.channel.resend(seq: 1), "a message not sent when the call ends is gone")
        XCTAssertEqual(h.channel.pendingCount("p1"), 0)
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertTrue(h.shown.isEmpty)
    }

    // MARK: - The text rule

    func testLineBreaksAreNormalisedBeforeTheRule() {
        XCTAssertEqual(CallChatText.normaliseLineBreaks("a\r\nb\rc\nd"), "a\nb\nc\nd")
        XCTAssertTrue(CallChatText.isSendable("a\r\nb"))
        XCTAssertTrue(CallChatText.isSendable("tab\there"))
    }

    func testEmptyBlankAndControlTextsCannotBeSent() {
        XCTAssertFalse(CallChatText.isSendable(""))
        XCTAssertFalse(CallChatText.isSendable("   \n\t"))
        XCTAssertFalse(CallChatText.isSendable("bell\u{7}"))
        XCTAssertTrue(CallChatText.isSendable(String(repeating: "a", count: 800)))
        XCTAssertFalse(CallChatText.isSendable(String(repeating: "a", count: 801)))
    }

    func testTheFieldTakesNoMoreTextAtTheLimitAndSaysSo() {
        let fits = CallChatText.edited(String(repeating: "a", count: 800))
        XCTAssertFalse(fits.tooLong)
        XCTAssertEqual(fits.text.utf8.count, 800)
        let over = CallChatText.edited(String(repeating: "a", count: 805))
        XCTAssertTrue(over.tooLong)
        XCTAssertEqual(over.text.utf8.count, 800)
        // Cyrillic: two bytes a letter, cut on a character boundary.
        let cyrillic = CallChatText.edited(String(repeating: "ж", count: 401))
        XCTAssertTrue(cyrillic.tooLong)
        XCTAssertEqual(cyrillic.text.count, 400)
    }

    // MARK: - The panel, the badge, the preview

    @MainActor
    private final class ModelHarness {
        let model = CallChatModel()
        var names: [String: String] = [:]
        var announced: [String] = []
        var scheduled: [(TimeInterval, @MainActor () -> Void)] = []
        var sends: [String] = []
        var sendWorks = true
        var resendWorks = true

        init() {
            model.tileName = { [unowned self] in self.names[$0] }
            model.announce = { [unowned self] in self.announced.append($0) }
            model.schedule = { [unowned self] seconds, action in self.scheduled.append((seconds, action)) }
            var seq: UInt64 = 0
            model.setActions(
                send: { [unowned self] text in
                    seq += 1
                    self.sends.append(text)
                    return (seq, self.sendWorks)
                },
                resend: { [unowned self] _ in self.resendWorks })
        }
    }

    func testTheBadgeCountsToNineThenNinePlus() {
        XCTAssertNil(CallChatModel.badgeText(unread: 0))
        XCTAssertEqual(CallChatModel.badgeText(unread: 1), "1")
        XCTAssertEqual(CallChatModel.badgeText(unread: 9), "9")
        XCTAssertEqual(CallChatModel.badgeText(unread: 10), "9+")
        XCTAssertEqual(CallChatModel.badgeText(unread: 250), "9+")
    }

    func testMessagesFromOthersAreCountedWhileThePanelIsClosedAndOwnNever() {
        let h = ModelHarness()
        for seq in 1...11 { h.model.receive(peerId: "p1", seq: UInt64(seq), text: "m\(seq)") }
        XCTAssertTrue(h.model.send("mine"))
        XCTAssertEqual(h.model.unread, 11, "an own message is never counted")
        XCTAssertEqual(CallChatModel.badgeText(unread: h.model.unread), "9+")
        h.model.openPanel()
        XCTAssertEqual(h.model.unread, 0, "opening the panel clears the count")
        h.model.receive(peerId: "p1", seq: 12, text: "while open")
        XCTAssertEqual(h.model.unread, 0)
        h.model.closePanel()
        h.model.receive(peerId: "p1", seq: 12, text: "while open")
        XCTAssertEqual(h.model.unread, 0, "the same message does not count twice")
    }

    func testAPreviewShowsFourSecondsAndANewOneReplacesIt() {
        let h = ModelHarness()
        h.model.receive(peerId: "p1", seq: 1, text: "first")
        XCTAssertEqual(h.model.preview?.text, "first")
        XCTAssertEqual(h.scheduled.first?.0, 4)
        h.model.receive(peerId: "p2", seq: 1, text: "second")
        XCTAssertEqual(h.model.preview?.text, "second")
        // The first one's timer runs out: the second is untouched.
        h.scheduled[0].1()
        XCTAssertEqual(h.model.preview?.text, "second")
        h.scheduled[1].1()
        XCTAssertNil(h.model.preview)
    }

    func testNoPreviewWhileThePanelIsOpenAndOpeningClearsOne() {
        let h = ModelHarness()
        h.model.receive(peerId: "p1", seq: 1, text: "first")
        h.model.openPanel()
        XCTAssertNil(h.model.preview)
        h.model.receive(peerId: "p1", seq: 2, text: "second")
        XCTAssertNil(h.model.preview)
        XCTAssertTrue(h.scheduled.count == 1)
    }

    func testAMessageWhileThePanelIsOpenIsAnnouncedWithItsSender() {
        let h = ModelHarness()
        h.names["p1"] = "Anna"
        h.model.openPanel()
        h.model.receive(peerId: "p1", seq: 1, text: "hello")
        // The catalog's formatter isolates each argument (FSI ... PDI).
        let isolates: Set<Character> = ["\u{2068}", "\u{2069}"]
        XCTAssertEqual(h.announced.map { $0.filter { !isolates.contains($0) } }, ["Anna: hello"])
        h.model.closePanel()
        h.model.receive(peerId: "p1", seq: 2, text: "closed")
        XCTAssertEqual(h.announced.count, 1, "with the panel closed only the count changes")
    }

    func testANewMessageWhileScrolledUpShowsThePillAndDoesNotMoveTheList() {
        let h = ModelHarness()
        h.model.openPanel()
        let before = h.model.scrollRequest
        h.model.receive(peerId: "p1", seq: 1, text: "at bottom")
        XCTAssertEqual(h.model.scrollRequest, before + 1)
        XCTAssertFalse(h.model.newBelow)
        h.model.setAtBottom(false)
        h.model.receive(peerId: "p1", seq: 2, text: "scrolled up")
        XCTAssertTrue(h.model.newBelow)
        XCTAssertEqual(h.model.scrollRequest, before + 1)
        h.model.setAtBottom(true)
        XCTAssertFalse(h.model.newBelow)
    }

    func testANotSentMessageIsSentAgainOnRequest() {
        let h = ModelHarness()
        h.sendWorks = false
        XCTAssertTrue(h.model.send("hi\r\nthere"))
        XCTAssertEqual(h.sends, ["hi\nthere"])
        XCTAssertEqual(h.model.messages.first?.delivery, .notSent)
        h.resendWorks = false
        h.model.sendAgain("own-1")
        XCTAssertEqual(h.model.messages.first?.delivery, .notSent)
        h.resendWorks = true
        h.model.sendAgain("own-1")
        XCTAssertEqual(h.model.messages.first?.delivery, .sent)
    }

    func testATextTheRuleRefusesIsNotSent() {
        let h = ModelHarness()
        XCTAssertFalse(h.model.send("   "))
        XCTAssertTrue(h.sends.isEmpty)
        XCTAssertTrue(h.model.messages.isEmpty)
    }

    func testTheNameOverABubbleFollowsTheTileAndOutlivesIt() {
        let h = ModelHarness()
        h.names["p1"] = "Anna"
        XCTAssertEqual(h.model.senderName("p1"), "Anna")
        h.names["p1"] = "Anna K"
        XCTAssertEqual(h.model.senderName("p1"), "Anna K", "a later change of the tile's name")
        h.names["p1"] = nil
        XCTAssertEqual(h.model.senderName("p1"), "Anna K", "the tile is gone: the last name stays")
    }

    func testANameStartsEachRunFromAnotherPerson() {
        let now = Date()
        func m(_ id: String, _ sender: CallChatMessage.Sender) -> CallChatMessage {
            CallChatMessage(id: id, sender: sender, seq: 1, text: id, arrivedAt: now, delivery: .sent)
        }
        let list = [m("a", .peer("p1")), m("b", .peer("p1")), m("c", .own), m("d", .peer("p1")),
                    m("e", .peer("p2"))]
        XCTAssertEqual(
            list.indices.map { CallChatModel.startsRun(at: $0, in: list) },
            [true, false, false, true, true])
    }

    func testTheCallsEndClearsTheChatAndAFreshCallHasNone() {
        let h = ModelHarness()
        h.model.receive(peerId: "p1", seq: 1, text: "hello")
        h.model.openPanel()
        h.model.callEnded()
        XCTAssertTrue(h.model.messages.isEmpty)
        XCTAssertFalse(h.model.panelOpen)
        XCTAssertEqual(h.model.unread, 0)
        XCTAssertTrue(h.model.available, "the screen still on show keeps its circle")
        h.model.reset()
        XCTAssertFalse(h.model.available)
        XCTAssertFalse(h.model.send("after"))
    }

    func testTheCirclesSpokenNameSaysTheCount() {
        XCTAssertEqual(CallChatModel.spokenCircleLabel(open: false, unread: 0), "Open chat")
        XCTAssertEqual(CallChatModel.spokenCircleLabel(open: true, unread: 3), "Close chat")
        XCTAssertEqual(CallChatModel.spokenCircleLabel(open: false, unread: 3), "Open chat, 3 unread")
    }

    func testTheColumnStartsAtNineHundredPoints() {
        XCTAssertFalse(CallChatModel.isColumn(screenWidth: 899))
        XCTAssertTrue(CallChatModel.isColumn(screenWidth: 900))
    }

    func testTheCallStateMirrorsTheChatsPanelAndAvailability() {
        let ui = CallUiState()
        XCTAssertFalse(ui.chatAvailable)
        ui.chat.setActions(send: { _ in (1, true) }, resend: { _ in true })
        XCTAssertTrue(ui.chatAvailable)
        ui.chat.openPanel()
        XCTAssertTrue(ui.chatPanelOpen)
        ui.reset()
        XCTAssertFalse(ui.chatAvailable)
        XCTAssertFalse(ui.chatPanelOpen)
    }

    func testTheTileNameOfASenderIsReadFromTheRoster() {
        let ui = CallUiState()
        ui.setParticipants([
            CallUiState.Participant(
                id: "peer-p1", displayName: "Mia", isLocal: false, micOn: true, camOn: false,
                handRaised: false, quality: .good, speaking: false, screenSharing: false),
        ])
        XCTAssertEqual(ui.chat.senderName("p1"), "Mia")
    }
}
