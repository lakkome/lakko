// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0092 / `docs/ux/attachments.md` — the iOS wiring of the attachment
/// menu and the content kinds: the menu rows come from the core rule, the
/// ballot queue waits 0..=10 s and survives a restart, a stored content
/// message reads back through the core, the folds read what the receive path
/// filed, and the open-items hand-over plans what a newcomer needs.
@MainActor
final class AttachmentKindsTests: XCTestCase {

    // MARK: Menu

    func testGroupMenuHasNineRowsInTheSpecOrderWithSections() {
        let rows = attachmentMenu(kind: .group, role: .member, hasCamera: true, hasLocation: true, callHoldsCamera: false)
        XCTAssertEqual(rows.map(\.item), [
            .photoOrVideo, .camera, .file, .audio, .poll, .checklist, .location, .contact, .article,
        ])
        XCTAssertTrue(rows.allSatisfy { $0.disabled == nil })
        // The core still groups the rows in four sections; the sheet draws
        // no separator between them (UX 2.2, amended 2026-11-18).
        XCTAssertEqual(Set(rows.map(\.section)).count, 4)
    }

    func testCameraRowIsDisabledWithItsReasonInACall() {
        let rows = attachmentMenu(kind: .chat, role: .member, hasCamera: true, hasLocation: true, callHoldsCamera: true)
        XCTAssertEqual(rows.first { $0.item == .camera }?.disabled, .cameraInCall)
        let noCamera = attachmentMenu(kind: .chat, role: .member, hasCamera: false, hasLocation: true, callHoldsCamera: false)
        XCTAssertEqual(noCamera.first { $0.item == .camera }?.disabled, .noCamera)
    }

    func testEveryMenuRowHasACatalogLabelAndIcon() {
        let all: [AttachmentMenuItem] = [
            .photoOrVideo, .camera, .file, .audio, .poll, .checklist, .location, .contact, .article,
        ]
        // One Lakko icon per row (design system section 8), none shared.
        XCTAssertEqual(Set(all.map(AttachmentMenuCopy.icon)).count, all.count)
        for item in all {
            let key = AttachmentMenuCopy.labelKey(item)
            XCTAssertTrue(key.hasPrefix("attach.menu."))
            XCTAssertNotEqual(Localization.text(key), key, "no catalog text for \(key)")
        }
        for reason: AttachmentMenuDisabled in [.noCamera, .cameraInCall, .noLocation] {
            let key = AttachmentMenuCopy.reasonKey(reason)
            XCTAssertNotEqual(Localization.text(key), key)
        }
    }

    func testMenuSheetIsSizedForFortyEightPointRows() {
        let rows = attachmentMenu(kind: .group, role: .member, hasCamera: true, hasLocation: true, callHoldsCamera: false)
        XCTAssertEqual(AttachmentMenuSheet.rowHeight, 48)
        XCTAssertEqual(AttachmentMenuSheet.sheetHeight(rows), 9 * 48 + 28)
        // A disabled row with its reason takes one more line.
        let inCall = attachmentMenu(kind: .group, role: .member, hasCamera: true, hasLocation: true, callHoldsCamera: true)
        XCTAssertEqual(AttachmentMenuSheet.sheetHeight(inCall), 9 * 48 + 16 + 28)
    }

    // MARK: Editor rows (UX 7.1a)

    func testAddRowIsAbsentAtTheMaximumAndRemoveAboveTheMinimumOnly() {
        XCTAssertTrue(EditorRows.canAdd(count: 9, max: 10))
        XCTAssertFalse(EditorRows.canAdd(count: 10, max: 10))
        XCTAssertFalse(EditorRows.canRemove(count: 2, min: 2))
        XCTAssertTrue(EditorRows.canRemove(count: 3, min: 2))
        XCTAssertFalse(EditorRows.canRemove(count: 1, min: 1))
        XCTAssertTrue(EditorRows.canRemove(count: 2, min: 1))
    }

    func testReturnMovesToTheNextRowAndAddsOneOnTheLast() {
        XCTAssertEqual(EditorRows.onReturn(at: 0, count: 3, max: 10), .focus(1))
        XCTAssertEqual(EditorRows.onReturn(at: 2, count: 3, max: 10), .append)
        XCTAssertEqual(EditorRows.onReturn(at: 9, count: 10, max: 10), .done)
    }

    func testMoveByOneStaysWithinTheList() {
        var rows = ["a", "b", "c"]
        XCTAssertEqual(EditorRows.move(&rows, at: 1, by: -1), 0)
        XCTAssertEqual(rows, ["b", "a", "c"])
        XCTAssertEqual(EditorRows.move(&rows, at: 1, by: 1), 2)
        XCTAssertEqual(rows, ["b", "c", "a"])
        XCTAssertNil(EditorRows.move(&rows, at: 0, by: -1))
        XCTAssertNil(EditorRows.move(&rows, at: 2, by: 1))
        XCTAssertEqual(rows, ["b", "c", "a"])
    }

    func testDragDestinationMatchesListMove() {
        for (source, offset) in [(0, 3), (2, 0), (1, 2), (1, 1), (0, 1)] {
            var rows = [0, 1, 2]
            rows.move(fromOffsets: IndexSet(integer: source), toOffset: offset)
            XCTAssertEqual(rows.firstIndex(of: source), EditorRows.destination(from: source, toOffset: offset))
        }
    }

    func testEditorCopyIsInTheCatalog() {
        for key in [
            "poll.editor.add_option", "checklist.editor.add_item", "checklist.editor.items", "poll.editor.options",
            "poll.editor.move_up", "poll.editor.move_down", "common.close",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, "no catalog text for \(key)")
        }
        // Text arguments arrive wrapped in direction isolates; strip them.
        let moved = Localization.shared.t("editor.moved", texts: ["n": "2"])
            .replacingOccurrences(of: "\u{2068}", with: "").replacingOccurrences(of: "\u{2069}", with: "")
        XCTAssertEqual(moved, "Moved to position 2.")
    }

    func testAnonymousSwitchIsAbsentInAOneToOneAndAGroupOfTwo() {
        XCTAssertEqual(pollAnonymity(kind: .chat, accounts: 2), .notOffered)
        XCTAssertEqual(pollAnonymity(kind: .group, accounts: 2), .notOffered)
        XCTAssertEqual(pollAnonymity(kind: .group, accounts: 5), .switchDefaultOn)
    }

    // MARK: Ballot queue

    func testBallotDelayIsWithinZeroToTenSeconds() {
        for random in [Int64(0), 1, 9_999, 10_000, 10_001, -3, Int64.max / 2] {
            let due = BallotSchedule.dueAt(nowMs: 1_000, random: random)
            XCTAssertGreaterThanOrEqual(due, 1_000)
            XCTAssertLessThanOrEqual(due, 11_000)
        }
        XCTAssertEqual(BallotSchedule.wait(dueAtMs: 5_000, nowMs: 6_000), 0)
        XCTAssertEqual(BallotSchedule.wait(dueAtMs: 5_000, nowMs: 1_000), 4_000)
    }

    func testBallotQueueWaitsPostsAndForgetsTheEntry() async throws {
        let account = "test-\(UUID().uuidString)"
        let queue = BallotQueue()
        var slept: [Int64] = []
        var posted: [QueuedBallot] = []
        queue.nowMs = { 100 }
        queue.randomMs = { 7_000 }
        queue.sleep = { slept.append($0) }
        queue.post = { posted.append($0) }
        queue.bind(chat: nil, accountIdB64: account)
        queue.enqueue(groupIdHex: "aa", pollMessageId: 5, choices: [1])
        XCTAssertEqual(queue.state(groupIdHex: "aa", pollMessageId: 5), .sending)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(slept.first, 7_000)
        XCTAssertEqual(posted.map(\.choices), [[1]])
        XCTAssertNil(queue.state(groupIdHex: "aa", pollMessageId: 5))
        XCTAssertTrue(BallotQueue.load(account).isEmpty)
        queue.reset()
    }

    func testAFailedBallotSurvivesARestartAndIsRetriedAtTheNextOpen() async throws {
        let account = "test-\(UUID().uuidString)"
        let first = BallotQueue()
        first.sleep = { _ in }
        first.post = { _ in throw ContentSendError.notReady }
        first.bind(chat: nil, accountIdB64: account)
        first.enqueue(groupIdHex: "bb", pollMessageId: 9, choices: [0, 2])
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(first.state(groupIdHex: "bb", pollMessageId: 9), .failed)
        XCTAssertEqual(BallotQueue.load(account).count, 1)

        // The next open: the queued ballot is sent again.
        let second = BallotQueue()
        var posted: [QueuedBallot] = []
        second.sleep = { _ in }
        second.post = { posted.append($0) }
        second.bind(chat: nil, accountIdB64: account)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(posted.map(\.choices), [[0, 2]])
        XCTAssertTrue(BallotQueue.load(account).isEmpty)
        second.reset()
    }

    func testANewVoteReplacesAQueuedOne() async throws {
        let account = "test-\(UUID().uuidString)"
        let queue = BallotQueue()
        var posted: [QueuedBallot] = []
        var gate: CheckedContinuation<Void, Never>?
        // The clock stands still and the delay is zero, so the replacing
        // entry is due when the first wait ends.
        queue.nowMs = { 0 }
        queue.randomMs = { 0 }
        var calls = 0
        // The first wait is held open until the second vote is queued; any
        // later wait returns at once, so nothing here can wait forever.
        queue.sleep = { _ in
            calls += 1
            if calls == 1 { await withCheckedContinuation { gate = $0 } }
        }
        queue.post = { posted.append($0) }
        queue.bind(chat: nil, accountIdB64: account)
        queue.enqueue(groupIdHex: "cc", pollMessageId: 1, choices: [0])
        try await Task.sleep(nanoseconds: 20_000_000)
        queue.enqueue(groupIdHex: "cc", pollMessageId: 1, choices: [3])
        gate?.resume()
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(posted.map(\.choices), [[3]])
        queue.reset()
    }

    // MARK: Content model

    func testAStoredPollReadsBackAndFoldsVotes() throws {
        let seed = Data(repeating: 7, count: 32)
        let identity = try Identity.fromSeed(seed: seed)
        let groupId = Data(repeating: 1, count: 32)
        let body = PollBody(
            id: Data(repeating: 2, count: 16), question: "Lunch?", options: ["Soup", "Salad"], multiple: false,
            quiz: nil, closeAtMs: nil, anonymous: nil)
        let signed = try signPoll(body: body, groupId: groupId, authorSeed: identity.ed25519Seed())
        var stored = try XCTUnwrap(ContentModel.store(content: .poll(poll: signed), attachments: []))
        let voter = Data(repeating: 9, count: 32)
        stored.ops.votes = [.init(messageId: 11, createdAtMs: 1, voterB64: voter.base64EncodedString(), choices: [1])]
        let message = ChatMessage(
            messageId: 10, direction: .outgoing, text: "", createdAt: "2026-11-15T10:00:00Z",
            authorKeyB64: identity.signaturePublicKey().base64EncodedString(), content: stored)
        let view = try XCTUnwrap(PollView.make(message: message, ownAccountKey: voter, registerChoice: nil, nowMs: 2))
        XCTAssertEqual(view.counts, [0, 1])
        XCTAssertEqual(view.voters, 1)
        XCTAssertEqual(view.ownChoices, [1])
        XCTAssertFalse(view.closed)
        XCTAssertEqual(ContentPreview.line(message)?.contains("Lunch?"), true)

        // It survives the message log and reads back the same.
        let persisted = PersistedMessage(from: message)
        let data = try JSONEncoder().encode(persisted)
        let back = try XCTUnwrap(JSONDecoder().decode(PersistedMessage.self, from: data).toChatMessage())
        XCTAssertEqual(back.content, message.content)
    }

    func testOnlyTheAuthorOrAnAdminClosesAnAnonymousPoll() {
        let author = Data(repeating: 1, count: 32)
        var ops = PersistedContentOps()
        ops.closes = [
            .init(messageId: 5, closerB64: Data(repeating: 2, count: 32).base64EncodedString(), closerIsAdmin: false),
            .init(messageId: 8, closerB64: author.base64EncodedString(), closerIsAdmin: false),
        ]
        XCTAssertEqual(PollView.validClose(ops: ops, author: author), 8)
        ops.closes[0].closerIsAdmin = true
        XCTAssertEqual(PollView.validClose(ops: ops, author: author), 5)
    }

    func testAChecklistFoldsTicksAndTheHandoverPlansIt() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 3, count: 32))
        let groupId = Data(repeating: 4, count: 32)
        let body = ChecklistBody(
            title: "Trip", othersAdd: true, othersMark: true,
            items: [ChecklistItem(id: 1, text: "Tickets"), ChecklistItem(id: 2, text: "Bags")])
        let signed = try signChecklist(body: body, groupId: groupId, authorSeed: identity.ed25519Seed())
        var stored = try XCTUnwrap(ContentModel.store(content: .checklist(checklist: signed), attachments: []))
        let other = Data(repeating: 6, count: 32)
        stored.ops.ticks = [.init(
            messageId: 21, tickerB64: other.base64EncodedString(), privileged: false,
            item: PersistedItemRef(authorId: 2), done: true)]
        let message = ChatMessage(
            messageId: 20, direction: .incoming, text: "", createdAt: "2026-11-15T10:00:00Z",
            authorKeyB64: identity.signaturePublicKey().base64EncodedString(), content: stored)
        let view = try XCTUnwrap(ChecklistView.make(message: message))
        XCTAssertEqual(view.doneCount, 1)
        XCTAssertTrue(view.mayTick(other))

        // ADR-0092 section 3.9 — the committer hands the open checklist over.
        let plan = ChatRepository.handoverPlan(messages: [message], nowMs: 1)
        XCTAssertEqual(plan.items.count, 1)
        XCTAssertEqual(plan.items.first?.pollRef(), 20)
        XCTAssertTrue(openItemsHandoverDue(addedLeaves: 1, heldItems: UInt32(plan.items.count)))
        let rows = try planOpenItemsRows(items: plan.items)
        XCTAssertFalse(rows.isEmpty)
        XCTAssertTrue(rows.allSatisfy { isOpenItemsEnvelope(bytes: $0) })
    }

    func testAClosedChecklistIsNotHandedOver() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 3, count: 32))
        let body = ChecklistBody(title: "T", othersAdd: false, othersMark: true, items: [ChecklistItem(id: 1, text: "a")])
        let signed = try signChecklist(body: body, groupId: Data(repeating: 4, count: 32), authorSeed: identity.ed25519Seed())
        var stored = try XCTUnwrap(ContentModel.store(content: .checklist(checklist: signed), attachments: []))
        let author = identity.signaturePublicKey().base64EncodedString()
        stored.ops.closes = [.init(messageId: 31, closerB64: author, closerIsAdmin: false)]
        let message = ChatMessage(
            messageId: 30, direction: .outgoing, text: "", createdAt: "2026-11-15T10:00:00Z",
            authorKeyB64: author, content: stored)
        XCTAssertTrue(ChecklistView.make(message: message)?.closed ?? false)
        XCTAssertTrue(ChatRepository.handoverPlan(messages: [message], nowMs: 1).items.isEmpty)
    }

    // MARK: Notification extension

    func testContentPreviewLinesNeverCarryJson() {
        XCTAssertEqual(nseContentPreviewLine(kind: .poll, text: "Lunch?").contains("Lunch?"), true)
        XCTAssertFalse(nseContentPreviewLine(kind: .location, text: "secret place").contains("secret"))
        XCTAssertFalse(nseContentPreviewLine(kind: .article, text: "").isEmpty)
    }

    // MARK: Channel polls

    func testAStaleChannelVoteIsRetriedOnceJustAfterTheStoredOne() {
        XCTAssertEqual(
            ChannelVoteRule.next(code: "stale-vote", storedAtMs: 1_000, serverTimeMs: nil, nowMs: 1_500, attempt: 0),
            .retry(atMs: 1_001))
        // Outside the 120 s window: not retried.
        XCTAssertEqual(
            ChannelVoteRule.next(code: "stale-vote", storedAtMs: 1_000, serverTimeMs: nil, nowMs: 500_000, attempt: 0),
            .fail)
        // A second retry never happens.
        XCTAssertEqual(
            ChannelVoteRule.next(code: "stale-vote", storedAtMs: 1_000, serverTimeMs: nil, nowMs: 1_500, attempt: 1),
            .fail)
        XCTAssertEqual(
            ChannelVoteRule.next(code: "vote-time-out-of-window", storedAtMs: nil, serverTimeMs: 9_000, nowMs: 1, attempt: 0),
            .retry(atMs: 9_000))
        XCTAssertEqual(
            ChannelVoteRule.next(code: "vote-proof-required", storedAtMs: nil, serverTimeMs: nil, nowMs: 1, attempt: 0),
            .fail)
    }

    func testBase64urlHasNoPaddingAndRoundTrips() {
        let bytes = Data([0xfb, 0xff, 0x01])
        let text = ChannelVoteRule.b64url(bytes)
        XCTAssertFalse(text.contains("=") || text.contains("+") || text.contains("/"))
        XCTAssertEqual(ChannelVoteRule.fromB64url(text), bytes)
    }

    func testAPollCloseInAChannelFreezesItsTallyAndTicksFoldIn() throws {
        let body = ChecklistBody(title: "T", othersAdd: false, othersMark: false, items: [ChecklistItem(id: 1, text: "a")])
        let post = try composeContentEnvelope(
            content: .checklist(checklist: SignedChecklist(body: body, authorSig: nil)), attachments: [],
            parts: ContentModel.emptyParts)
        let tick = try composeChecklistTickEnvelope(target: 1, item: .author(id: 1), done: true)
        func decoded(_ cursor: UInt64, _ bytes: Data) -> ChannelDecodedPost {
            ChannelDecodedPost(
                cursor: cursor, serverTsMs: 1, epoch: 1,
                state: .body(ChannelPostBody.read(String(decoding: bytes, as: UTF8.self))))
        }
        let folded = ChannelTimeline.fold([decoded(1, post), decoded(2, tick)])
        XCTAssertEqual(folded.items.count, 1)
        let view = try XCTUnwrap(ChecklistView.make(message: ChannelTimeline.message(folded.items[0])))
        XCTAssertEqual(view.doneCount, 1)
    }

    // MARK: Send preview, article, admitters

    func testTheVideoRuleIsMp4H264UnderTheCap() {
        XCTAssertTrue(ComposerTrayUpload.videoAcceptable(isMP4: true, isH264: true, bytes: 1_000))
        XCTAssertFalse(ComposerTrayUpload.videoAcceptable(isMP4: false, isH264: true, bytes: 1_000))
        XCTAssertFalse(ComposerTrayUpload.videoAcceptable(isMP4: true, isH264: false, bytes: 1_000))
        XCTAssertFalse(ComposerTrayUpload.videoAcceptable(
            isMP4: true, isH264: true, bytes: AttachmentBridge.maxAttachmentBytes + 1))
    }

    func testArticleRunsAreCanonicalAndCarryMarksOnTheSelection() {
        var block = ArticleDraftBlock()
        block.runs = [
            ArticleDraftRun(text: " Hello "), ArticleDraftRun(text: "bold", bold: true),
            ArticleDraftRun(text: "er", bold: true), ArticleDraftRun(text: ""),
            ArticleDraftRun(text: " link ", link: "https://example.org"),
        ]
        let runs = ArticleDraft.runs(block)
        XCTAssertEqual(runs, [
            .text(bold: false, italic: false, text: "Hello "),
            .text(bold: true, italic: false, text: "bolder"),
            .link(url: "https://example.org", bold: false, italic: false, text: " link"),
        ])
        var draft = ArticleDraft()
        draft.title = "Title"
        draft.blocks = [block]
        XCTAssertNotNil(draft.build())
    }

    func testTheAdmitterIsKeptPerAccountAndGroup() {
        let account = "test-\(UUID().uuidString)"
        let key = Data(repeating: 5, count: 32)
        ContentAdmitters.save(accountIdB64: account, groupIdHex: "ab", key: key)
        XCTAssertEqual(ContentAdmitters.load(accountIdB64: account, groupIdHex: "ab"), key)
        XCTAssertNil(ContentAdmitters.load(accountIdB64: account, groupIdHex: "cd"))
        UserDefaults.standard.removeObject(forKey: ContentAdmitters.key(account))
    }
}
