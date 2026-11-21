// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
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

    func testAnEmojiGoesAtTheEndOfTheDraft() {
        let picked = CallChatText.appendingEmoji("\u{1F600}", to: "hi ")
        XCTAssertEqual(picked.text, "hi \u{1F600}")
        XCTAssertFalse(picked.tooLong)
        let empty = CallChatText.appendingEmoji("\u{1F44D}", to: "")
        XCTAssertEqual(empty.text, "\u{1F44D}")
        XCTAssertFalse(empty.tooLong)
    }

    func testAnEmojiThatWouldPassTheLimitIsNotPutIn() {
        // 797 bytes + a 4-byte emoji = 801: refused whole, the draft unchanged.
        let draft = String(repeating: "a", count: 797)
        let refused = CallChatText.appendingEmoji("\u{1F600}", to: draft)
        XCTAssertEqual(refused.text, draft)
        XCTAssertTrue(refused.tooLong)
        // 796 bytes + 4 = 800: fits exactly.
        let exact = CallChatText.appendingEmoji("\u{1F600}", to: String(repeating: "a", count: 796))
        XCTAssertEqual(exact.text.utf8.count, 800)
        XCTAssertFalse(exact.tooLong)
        // A long ZWJ sequence is never cut in part.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"
        let longDraft = String(repeating: "a", count: 790)
        let refusedFamily = CallChatText.appendingEmoji(family, to: longDraft)
        XCTAssertEqual(refusedFamily.text, longDraft)
        XCTAssertTrue(refusedFamily.tooLong)
    }

    func testAnEmojiGoesAtTheCaretInTheMiddle() {
        // "hello world", caret after "hello " (offset 6).
        let put = CallChatText.insertingEmoji("\u{1F600}", into: "hello world", range: 6..<6)
        XCTAssertEqual(put.text, "hello \u{1F600}world")
        XCTAssertFalse(put.tooLong)
        // The caret moves after the emoji (two UTF-16 units).
        XCTAssertEqual(put.caret, 8)
    }

    func testAnEmojiReplacesTheSelection() {
        // "hello world" with "world" selected (offsets 6..<11).
        let put = CallChatText.insertingEmoji("\u{1F44D}", into: "hello world", range: 6..<11)
        XCTAssertEqual(put.text, "hello \u{1F44D}")
        XCTAssertFalse(put.tooLong)
        XCTAssertEqual(put.caret, 8)
        // A selection after an earlier emoji counts UTF-16 units.
        let after = CallChatText.insertingEmoji("!", into: "\u{1F600}ab", range: 2..<3)
        XCTAssertEqual(after.text, "\u{1F600}!b")
        XCTAssertEqual(after.caret, 3)
    }

    func testAnEmojiAtTheCaretThatWouldPassTheLimitIsNotPutIn() {
        let draft = String(repeating: "a", count: 797)
        let refused = CallChatText.insertingEmoji("\u{1F600}", into: draft, range: 100..<100)
        XCTAssertEqual(refused.text, draft)
        XCTAssertTrue(refused.tooLong)
        XCTAssertNil(refused.caret)
        // Replacing a selection frees room: 800 bytes, 4 replaced by 4.
        let full = String(repeating: "a", count: 800)
        let replaced = CallChatText.insertingEmoji("\u{1F600}", into: full, range: 0..<4)
        XCTAssertEqual(replaced.text.utf8.count, 800)
        XCTAssertFalse(replaced.tooLong)
        XCTAssertEqual(replaced.caret, 2)
    }

    func testAnEmojiWithNoUsableCaretGoesAtTheEnd() {
        // No caret (iOS 17), a stale range, and one inside a character.
        XCTAssertEqual(CallChatText.insertingEmoji("\u{1F600}", into: "hi", range: nil).text, "hi\u{1F600}")
        XCTAssertEqual(CallChatText.insertingEmoji("\u{1F600}", into: "hi", range: 5..<5).text, "hi\u{1F600}")
        let inside = CallChatText.insertingEmoji("x", into: "\u{1F600}", range: 1..<1)
        XCTAssertEqual(inside.text, "\u{1F600}x")
        XCTAssertEqual(inside.caret, 3)
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

    // MARK: - Pictures (section 4.1.4.1)

    private func picture(_ byte: UInt8, width: UInt16 = 400, height: UInt16 = 300) -> CallChatPicture {
        CallChatPicture(
            blobId: Data(repeating: byte, count: 32), key: Data(repeating: 2, count: 32),
            nonce: Data(repeating: 3, count: 12), size: 1000, width: width, height: height)
    }

    private func prepared(_ byte: UInt8, width: UInt16 = 400, height: UInt16 = 300) -> CallChatPreparedPicture {
        CallChatPreparedPicture(jpeg: Data([0xFF, 0xD8, byte]), width: width, height: height)
    }

    /// Lets the model's main-actor tasks run until `condition` holds.
    private func settle(_ condition: () -> Bool) async {
        for _ in 0..<500 where !condition() { await Task.yield() }
    }

    /// A model wired for pictures over fakes: what went out, what was
    /// uploaded, what was deleted.
    @MainActor
    private final class PictureHarness {
        let model = CallChatModel()
        var seq: UInt64 = 0
        var sent: [(content: CallChatChannel.Content, seq: UInt64)] = []
        var sendWorks = true
        var uploads = 0
        var uploadFails = false
        var uploadGate: CheckedContinuation<Void, Never>?
        var holdUploads = false
        var nextBlob: UInt8 = 0xA0
        var deleted: [[Data]] = []
        var downloadData: Data?

        init() {
            model.schedule = { _, _ in }
            model.announce = { _ in }
            model.setActions(
                send: { [unowned self] text in
                    self.seq += 1
                    if self.sendWorks { self.sent.append((.text(text), self.seq)) }
                    return (self.seq, self.sendWorks)
                },
                resend: { _ in true })
            model.setPictureActions(
                reserveSeq: { [unowned self] in
                    self.seq += 1
                    return self.seq
                },
                send: { [unowned self] content, seq in
                    guard self.sendWorks else { return false }
                    self.sent.append((content, seq))
                    return true
                })
            model.pictureIO = CallChatPictureIO(
                upload: { [unowned self] prepared in
                    self.uploads += 1
                    if self.holdUploads {
                        await withCheckedContinuation { self.uploadGate = $0 }
                    }
                    if self.uploadFails { throw CancellationError() }
                    self.nextBlob += 1
                    return CallChatPicture(
                        blobId: Data(repeating: self.nextBlob, count: 32), key: Data(repeating: 2, count: 32),
                        nonce: Data(repeating: 3, count: 12), size: UInt32(prepared.jpeg.count),
                        width: prepared.width, height: prepared.height)
                },
                download: { [unowned self] _ in
                    guard let data = self.downloadData else { throw CancellationError() }
                    return data
                },
                deleteUploaded: { [unowned self] ids in self.deleted.append(ids) })
        }

        func releaseUpload() {
            let gate = uploadGate
            uploadGate = nil
            gate?.resume()
        }
    }

    func testTheTrayHoldsOnePictureASecondPasteReplacesIt() {
        let h = PictureHarness()
        XCTAssertFalse(h.model.canSend(""))
        h.model.pasted(.success(prepared(1)))
        XCTAssertEqual(h.model.trayPicture, prepared(1))
        XCTAssertTrue(h.model.canSend(""), "a picture alone can be sent")
        XCTAssertTrue(h.model.canSend("   "))
        h.model.pasted(.success(prepared(2)))
        XCTAssertEqual(h.model.trayPicture, prepared(2), "a second paste replaces the first")
        // Something that is not a picture: the line shows, the tray stays.
        h.model.pasted(.failure(.notPicture))
        XCTAssertEqual(h.model.trayPicture, prepared(2))
        XCTAssertEqual(h.model.pictureRefusal, .notPicture)
        XCTAssertEqual(CallChatPictures.Refusal.notPicture.textKey, "call.chat.picture_only")
        XCTAssertEqual(CallChatPictures.Refusal.tooLarge.textKey, "call.chat.picture_too_large")
        // The next good paste clears the line.
        h.model.pasted(.success(prepared(3)))
        XCTAssertNil(h.model.pictureRefusal)
        h.model.removeTrayPicture()
        XCTAssertNil(h.model.trayPicture)
        XCTAssertFalse(h.model.canSend(""))
    }

    func testTheBubbleFitsIntoTwoFortyAndKeepsEightyOnEachSide() {
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 4000, height: 3000), CGSize(width: 240, height: 180))
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 3000, height: 4000), CGSize(width: 180, height: 240))
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 2560, height: 2560), CGSize(width: 240, height: 240))
        // Never enlarged, but at least 80 on each side.
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 100, height: 50), CGSize(width: 100, height: 80))
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 40, height: 40), CGSize(width: 80, height: 80))
        // A very long picture is cropped by an 80-high box.
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 2560, height: 100), CGSize(width: 240, height: 80))
        XCTAssertEqual(CallChatPictures.bubbleSize(width: 0, height: 10), CGSize(width: 240, height: 240))
    }

    func testThePictureGoesFirstAndTheTextFollows() {
        XCTAssertEqual(
            CallChatPictures.parts(draft: "hello", hasPicture: true), [.picture, .text("hello")])
        XCTAssertEqual(CallChatPictures.parts(draft: "  \n ", hasPicture: true), [.picture])
        XCTAssertEqual(CallChatPictures.parts(draft: "hi", hasPicture: false), [.text("hi")])
        XCTAssertEqual(CallChatPictures.parts(draft: " ", hasPicture: false), [])
    }

    func testTheTextWaitsForThePictureAndGoesOutAfterIt() async {
        let h = PictureHarness()
        h.holdUploads = true
        h.model.pasted(.success(prepared(1)))
        XCTAssertTrue(h.model.send("after"))
        XCTAssertNil(h.model.trayPicture, "the tray empties on Send")
        // Both bubbles show at once, the picture first, both on their way.
        XCTAssertEqual(h.model.messages.map(\.seq), [1, 2])
        XCTAssertNotNil(h.model.messages[0].picture)
        XCTAssertEqual(h.model.messages[1].text, "after")
        XCTAssertEqual(h.model.messages.map(\.delivery), [.sending, .sending])
        await settle { h.uploadGate != nil }
        XCTAssertTrue(h.sent.isEmpty, "nothing goes out before the picture's file is up")
        h.releaseUpload()
        await settle { h.sent.count == 2 }
        XCTAssertEqual(h.sent.map(\.seq), [1, 2])
        guard case .picture(let sentPicture) = h.sent[0].content else {
            return XCTFail("the picture goes out first")
        }
        XCTAssertEqual(sentPicture.width, 400)
        XCTAssertEqual(h.sent[1].content, .text("after"))
        XCTAssertEqual(h.model.messages.map(\.delivery), [.sent, .sent])
    }

    func testAPictureAndATextShareOneCounter() async {
        let h = PictureHarness()
        XCTAssertTrue(h.model.send("one"))
        h.model.pasted(.success(prepared(1)))
        XCTAssertTrue(h.model.send(""))
        XCTAssertTrue(h.model.send("three"))
        await settle { h.sent.count == 3 }
        XCTAssertEqual(h.model.messages.map(\.seq), [1, 2, 3])
    }

    func testAFailedUploadIsNotSentAndSendAgainUploadsAgain() async {
        let h = PictureHarness()
        h.uploadFails = true
        h.model.pasted(.success(prepared(1)))
        XCTAssertTrue(h.model.send("text"))
        await settle { h.model.messages.allSatisfy { $0.delivery != .sending } }
        XCTAssertEqual(h.model.messages.map(\.delivery), [.notSent, .sent], "the text still follows")
        XCTAssertEqual(h.sent.map(\.seq), [2])
        h.uploadFails = false
        h.model.sendAgain(h.model.messages[0].id)
        XCTAssertEqual(h.model.messages[0].delivery, .sending)
        await settle { h.model.messages[0].delivery == .sent }
        XCTAssertEqual(h.uploads, 2, "the file is uploaded again")
        XCTAssertEqual(h.sent.map(\.seq), [2, 1])
        XCTAssertEqual(h.model.uploadedBlobIds.count, 1)
    }

    func testEveryUploadOfThisCallIsDeletedWhenItEnds() async {
        let h = PictureHarness()
        for byte in [UInt8(1), 2] {
            h.model.pasted(.success(prepared(byte)))
            XCTAssertTrue(h.model.send(""))
        }
        await settle { h.sent.count == 2 }
        let uploaded = h.model.uploadedBlobIds
        XCTAssertEqual(uploaded.count, 2)
        h.model.callEnded()
        XCTAssertEqual(h.deleted, [uploaded], "one owed delete with every upload of the call")
        XCTAssertTrue(h.model.uploadedBlobIds.isEmpty)
        XCTAssertTrue(h.model.messages.isEmpty)
        XCTAssertTrue(h.model.pictureImages.isEmpty)
        // Ending again deletes nothing more.
        h.model.callEnded()
        XCTAssertEqual(h.deleted.count, 1)
    }

    func testAnUploadThatFinishesAfterTheCallEndedIsDeletedAtOnce() async {
        let h = PictureHarness()
        h.holdUploads = true
        h.model.pasted(.success(prepared(1)))
        XCTAssertTrue(h.model.send("late"))
        await settle { h.uploadGate != nil }
        h.model.callEnded()
        XCTAssertTrue(h.deleted.isEmpty, "nothing was up yet")
        h.releaseUpload()
        await settle { !h.deleted.isEmpty }
        XCTAssertEqual(h.deleted.count, 1)
        XCTAssertEqual(h.deleted[0].count, 1)
        XCTAssertTrue(h.sent.isEmpty, "nothing goes out after the call ended")
        XCTAssertTrue(h.model.messages.isEmpty)
    }

    func testAReceivedPictureLoadsOrIsNotAvailable() async throws {
        let h = PictureHarness()
        h.downloadData = try XCTUnwrap(UIGraphicsImageRenderer(size: CGSize(width: 20, height: 10))
            .jpegData(withCompressionQuality: 0.8) { ctx in
                UIColor.red.setFill()
                ctx.fill(CGRect(x: 0, y: 0, width: 20, height: 10))
            })
        h.model.receivePicture(peerId: "p1", seq: 1, picture: picture(1))
        XCTAssertEqual(h.model.messages.first?.picture?.state, .loading)
        XCTAssertEqual(h.model.unread, 1)
        XCTAssertEqual(h.model.preview?.text, "Picture")
        await settle { h.model.messages.first?.picture?.state != .loading }
        XCTAssertEqual(h.model.messages.first?.picture?.state, .ready)
        XCTAssertNotNil(h.model.pictureImages["p1-1"])
        XCTAssertEqual(h.model.pictureData("p1-1"), h.downloadData)
        // The same message once.
        h.model.receivePicture(peerId: "p1", seq: 1, picture: picture(1))
        XCTAssertEqual(h.model.messages.count, 1)
        // One that cannot be downloaded.
        h.downloadData = nil
        h.model.receivePicture(peerId: "p1", seq: 2, picture: picture(2))
        await settle { h.model.messages.last?.picture?.state != .loading }
        XCTAssertEqual(h.model.messages.last?.picture?.state, .unavailable)
        XCTAssertNil(h.model.pictureImages["p1-2"])
    }

    func testAPastedPictureIsReencodedAsJPEGAndBroughtDown() throws {
        let png = UIGraphicsImageRenderer(
            size: CGSize(width: 4000, height: 100), format: {
                let format = UIGraphicsImageRendererFormat()
                format.scale = 1
                return format
            }()
        ).pngData { ctx in
            UIColor.blue.setFill()
            ctx.fill(CGRect(x: 0, y: 0, width: 4000, height: 100))
        }
        guard case .success(let ready) = CallChatPictures.prepare(png, maxSide: 2560) else {
            return XCTFail("a PNG is a picture")
        }
        XCTAssertEqual(Array(ready.jpeg.prefix(2)), [0xFF, 0xD8], "re-encoded as JPEG")
        XCTAssertEqual(ready.width, 2560)
        XCTAssertEqual(ready.height, 64)
        XCTAssertEqual(CallChatPictures.prepare(Data("not a picture".utf8)), .failure(.notPicture))
        XCTAssertEqual(CallChatPictures.prepare(png, maxSide: 2560, maxBytes: 10), .failure(.tooLarge))
    }

    func testAChannelPictureIsComposedAndReadBackUnderTheSharedCounter() {
        var composed: [(UInt64, CallChatPicture)] = []
        var shown: [CallChatChannel.Verified] = []
        let pic = picture(9)
        let channel = CallChatChannel(deps: CallChatChannel.Deps(
            compose: { _, seq, text in Data("t|\(seq)|\(text)".utf8) },
            verify: { _, _, bytes in
                bytes == Data("pic".utf8)
                    ? CallChatChannel.Verified(seq: 4, text: "", picture: pic) : nil
            },
            sendSealed: { _ in true },
            selfPeerId: { "me" },
            onMessage: { _, message in shown.append(message) },
            composePicture: { _, seq, picture in
                composed.append((seq, picture))
                return Data("p|\(seq)".utf8)
            }))
        XCTAssertEqual(channel.send("hi").seq, 1)
        let seq = channel.reserveSeq()
        XCTAssertEqual(seq, 2)
        XCTAssertTrue(channel.send(.picture(pic), seq: seq))
        XCTAssertEqual(composed.map(\.0), [2])
        XCTAssertEqual(channel.send("after").seq, 3)
        channel.peerIdentified("p1", signerKey: Data(repeating: 7, count: 32))
        channel.received(from: "p1", bytes: Data("pic".utf8))
        XCTAssertEqual(shown.first?.picture, pic)
    }
}
