// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0073 R.7 case 1 - the durable queue of held messages. The stored-form
/// cases are those of `apps/web/src/chat/held-send-queue.test.ts` and the
/// queue half of `held-reply-and-flush.test.ts`; the rest pins what the file
/// layout adds on iOS (order, removal, the two clears, the sealer).
final class HeldSendQueueTests: XCTestCase {

    private var base: URL!
    private let owner = "b3duZXI/YWNjb3VudCs="
    private let conversation = String(repeating: "ab", count: 32)
    private let otherConversation = String(repeating: "cd", count: 32)

    /// A reversible stand-in for the device-key wrap: what is on disk is not
    /// the JSON.
    private static let xor = HeldSendQueue.Sealer(
        seal: { Data($0.map { $0 ^ 0x5a }) },
        open: { Data($0.map { $0 ^ 0x5a }) })

    private struct SealFailed: Error {}

    override func setUpWithError() throws {
        try super.setUpWithError()
        base = FileManager.default.temporaryDirectory
            .appendingPathComponent("held-send-queue-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        if let base { try? FileManager.default.removeItem(at: base) }
        base = nil
        try super.tearDownWithError()
    }

    private func queue(owner: String? = nil, sealer: HeldSendQueue.Sealer = HeldSendQueueTests.xor) -> HeldSendQueue {
        HeldSendQueue(ownerAccountId: owner ?? self.owner, sealer: sealer, baseDirectory: base)
    }

    private func entry(_ localId: String, _ text: String = "hi", at ms: Int64 = 1) -> HeldSendQueue.Entry {
        HeldSendQueue.Entry(localId: localId, queuedAtMs: ms, text: text)
    }

    private func fileURL(_ conversationHex: String) -> URL {
        base.appendingPathComponent("HeldSend", isDirectory: true)
            .appendingPathComponent(HeldSendQueue.accountDirectoryName(owner), isDirectory: true)
            .appendingPathComponent(conversationHex)
            .appendingPathExtension("held")
    }

    // MARK: - the stored form (held-send-queue.test.ts)

    func testTextAnUploadedAttachmentAReplyAndAForwardRoundTrip() {
        let attachment = Data([1, 2, 3, 4])
        let forward = Data("{\"author\":\"Bob\"}".utf8)
        let stored = HeldSendQueue.Entry(
            localId: "l1", queuedAtMs: 99, text: "hello", attachments: [attachment],
            reply: HeldSend.Reply(target: 7, author: "Anna", excerpt: "see you"),
            replyGroupIdB64: "b2xk", forward: forward)
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: stored))
        // A second instance reads the disk, not this one's memory.
        let read = queue().list(conversationHex: conversation)
        XCTAssertEqual(read, [stored])
        XCTAssertEqual(read.first?.queuedAtMs, 99)
        XCTAssertEqual(read.first?.text, "hello")
        XCTAssertEqual(read.first?.attachments, [attachment])
        XCTAssertEqual(read.first?.reply, HeldSend.Reply(target: 7, author: "Anna", excerpt: "see you"))
        XCTAssertEqual(read.first?.forward, forward)
    }

    func testAPlainTypedMessageStoresNoReplyAndNoForward() throws {
        let plain = entry("l1", "hi")
        XCTAssertNil(plain.reply)
        XCTAssertNil(plain.replyGroupIdB64)
        XCTAssertNil(plain.forward)
        XCTAssertTrue(plain.attachments.isEmpty)
        let json = try XCTUnwrap(String(data: JSONEncoder().encode(plain), encoding: .utf8))
        XCTAssertFalse(json.contains("reply"))
        XCTAssertFalse(json.contains("forward"))
    }

    func testJunkIsDroppedNeverSent() {
        let q = queue()
        // Not a conversation id.
        XCTAssertFalse(q.enqueue(conversationHex: "not hex", entry: entry("l1")))
        XCTAssertFalse(q.enqueue(conversationHex: "", entry: entry("l1")))
        XCTAssertFalse(q.enqueue(conversationHex: "ABCD", entry: entry("l1")))
        XCTAssertFalse(q.enqueue(conversationHex: "../ab", entry: entry("l1")))
        // Not a message.
        XCTAssertFalse(q.enqueue(conversationHex: conversation, entry: entry("l1", "")))
        XCTAssertFalse(q.enqueue(conversationHex: conversation, entry: entry("", "x")))
        XCTAssertTrue(q.all().isEmpty)
        XCTAssertTrue(q.list(conversationHex: "not hex").isEmpty)
    }

    func testAMediaMessageWithNoTextIsAMessage() {
        let media = HeldSendQueue.Entry(localId: "l1", queuedAtMs: 1, text: "", attachments: [Data([9])])
        XCTAssertTrue(media.isValid)
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: media))
        XCTAssertEqual(queue().list(conversationHex: conversation), [media])
    }

    func testAStoredRowThatIsNotAMessageIsDroppedOnRead() throws {
        // Written past the queue: one row with no text and no attachment,
        // one repeated local id, one good row.
        let body = """
        {"version":1,"entries":[
          {"localId":"empty","queuedAtMs":1,"text":"","attachments":[]},
          {"localId":"good","queuedAtMs":2,"text":"x","attachments":[]},
          {"localId":"good","queuedAtMs":3,"text":"again","attachments":[]}
        ]}
        """
        let url = fileURL(conversation)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Self.xor.seal(Data(body.utf8)).write(to: url)
        XCTAssertEqual(queue().list(conversationHex: conversation), [entry("good", "x", at: 2)])
    }

    func testAFileThatDoesNotOpenOrParseReadsAsNothingHeld() throws {
        let url = fileURL(conversation)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("not json at all".utf8).write(to: url)
        XCTAssertTrue(queue().list(conversationHex: conversation).isEmpty)
        // An unknown version is not read either.
        try Self.xor.seal(Data("{\"version\":2,\"entries\":[]}".utf8)).write(to: url)
        XCTAssertTrue(queue().list(conversationHex: conversation).isEmpty)
        // A sealer that cannot open reads nothing and does not throw.
        XCTAssertTrue(queue().enqueue(conversationHex: otherConversation, entry: entry("l1")))
        let closed = HeldSendQueue.Sealer(seal: { $0 }, open: { _ in throw SealFailed() })
        XCTAssertTrue(queue(sealer: closed).list(conversationHex: otherConversation).isEmpty)
    }

    // MARK: - the compose group of a held reply (held-reply-and-flush.test.ts)

    func testTheComposeGroupOfAHeldReplySurvivesARelaunch() {
        let held = HeldSendQueue.Entry(
            localId: "l1", queuedAtMs: 1, text: "hi",
            reply: HeldSend.Reply(target: 7, author: "Anna", excerpt: "see you"),
            replyGroupIdB64: "b2xk")
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: held))
        let read = queue().list(conversationHex: conversation).first
        XCTAssertEqual(read?.replyGroupIdB64, "b2xk")
        XCTAssertEqual(read?.reply?.target, 7)
    }

    func testAMessageWithNoReplyStoresNoGroup() throws {
        let held = HeldSendQueue.Entry(
            localId: "l1", queuedAtMs: 1, text: "hi", reply: nil, replyGroupIdB64: "b2xk")
        XCTAssertNil(held.replyGroupIdB64)
        let json = try XCTUnwrap(String(data: JSONEncoder().encode(held), encoding: .utf8))
        XCTAssertFalse(json.contains("replyGroupIdB64"))
        // An empty group is none.
        let empty = HeldSendQueue.Entry(
            localId: "l2", queuedAtMs: 1, text: "hi",
            reply: HeldSend.Reply(target: 7, author: nil, excerpt: nil), replyGroupIdB64: "")
        XCTAssertNil(empty.replyGroupIdB64)
    }

    // MARK: - the queue

    func testListsInTheOrderQueuedPerConversation() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a", "first", at: 10)))
        XCTAssertTrue(q.enqueue(conversationHex: otherConversation, entry: entry("x", "other", at: 5)))
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("b", "second", at: 3)))
        XCTAssertEqual(q.list(conversationHex: conversation).map(\.text), ["first", "second"])
        XCTAssertEqual(q.count(conversationHex: conversation), 2)
        XCTAssertEqual(q.count(conversationHex: otherConversation), 1)
    }

    func testARepeatedEnqueueKeepsTheFirst() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a", "first")))
        XCTAssertFalse(q.enqueue(conversationHex: conversation, entry: entry("a", "again")))
        XCTAssertEqual(q.list(conversationHex: conversation).map(\.text), ["first"])
    }

    func testRemovesOneAfterAConfirmedSendAndKeepsTheOrderOfTheRest() {
        let q = queue()
        for (id, text) in [("a", "1"), ("b", "2"), ("c", "3")] {
            XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry(id, text)))
        }
        XCTAssertTrue(q.remove(conversationHex: conversation, localId: "b"))
        XCTAssertEqual(queue().list(conversationHex: conversation).map(\.localId), ["a", "c"])
        // Removing what is not held is not a failure.
        XCTAssertTrue(q.remove(conversationHex: conversation, localId: "b"))
        XCTAssertTrue(q.remove(conversationHex: otherConversation, localId: "a"))
        XCTAssertEqual(q.list(conversationHex: conversation).map(\.localId), ["a", "c"])
    }

    func testRemovingTheLastLeavesNoFile() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertTrue(FileManager.default.fileExists(atPath: fileURL(conversation).path))
        XCTAssertTrue(q.remove(conversationHex: conversation, localId: "a"))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fileURL(conversation).path))
        XCTAssertTrue(q.all().isEmpty)
    }

    func testClearsOneConversationAndLeavesTheOther() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertTrue(q.enqueue(conversationHex: otherConversation, entry: entry("x")))
        XCTAssertTrue(q.clear(conversationHex: conversation))
        XCTAssertTrue(q.list(conversationHex: conversation).isEmpty)
        XCTAssertEqual(q.list(conversationHex: otherConversation).map(\.localId), ["x"])
        // Clearing what holds nothing is not a failure.
        XCTAssertTrue(q.clear(conversationHex: conversation))
    }

    func testClearsOneAccountAndLeavesTheOther() {
        let mine = queue()
        let theirs = queue(owner: "b3RoZXI=")
        XCTAssertTrue(mine.enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertTrue(mine.enqueue(conversationHex: otherConversation, entry: entry("b")))
        XCTAssertTrue(theirs.enqueue(conversationHex: conversation, entry: entry("z")))
        XCTAssertTrue(mine.clearAccount())
        XCTAssertTrue(mine.all().isEmpty)
        XCTAssertEqual(theirs.list(conversationHex: conversation).map(\.localId), ["z"])
        XCTAssertTrue(HeldSendQueue.clearAccount(ownerAccountId: "b3RoZXI=", baseDirectory: base))
        XCTAssertTrue(theirs.all().isEmpty)
        // An account with nothing held clears cleanly.
        XCTAssertTrue(HeldSendQueue.clearAccount(ownerAccountId: "bm9uZQ==", baseDirectory: base))
    }

    func testOneAccountNeverReadsAnothers() {
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertTrue(queue(owner: "b3RoZXI=").list(conversationHex: conversation).isEmpty)
    }

    func testListsEveryConversationTheLongestWaitingFirst() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a", "late", at: 20)))
        XCTAssertTrue(q.enqueue(conversationHex: otherConversation, entry: entry("x", "early", at: 10)))
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("b", "later", at: 30)))
        let all = q.all()
        XCTAssertEqual(all.map(\.conversationHex), [otherConversation, conversation])
        XCTAssertEqual(all[1].entries.map(\.localId), ["a", "b"])
    }

    func testARelaunchRestartsTheGiveUpClockAndLeavesTheDiskAlone() {
        let q = queue()
        XCTAssertTrue(q.enqueue(conversationHex: conversation, entry: entry("a", "x", at: 5)))
        let restored = queue().restored(nowMs: 9000)
        XCTAssertEqual(restored.map(\.conversationHex), [conversation])
        XCTAssertEqual(restored.first?.entries.map(\.queuedAtMs), [9000])
        XCTAssertEqual(restored.first?.entries.map(\.text), ["x"])
        XCTAssertEqual(q.list(conversationHex: conversation).map(\.queuedAtMs), [5])
    }

    // MARK: - at rest

    func testWhatIsOnDiskIsTheSealedFormNotThePlaintext() throws {
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: entry("a", "a-secret-sentence")))
        let onDisk = try Data(contentsOf: fileURL(conversation))
        XCTAssertNil(onDisk.range(of: Data("a-secret-sentence".utf8)))
        let opened = try Self.xor.open(onDisk)
        XCTAssertNotNil(opened.range(of: Data("a-secret-sentence".utf8)))
    }

    func testAFailedSealStoresNothing() {
        let failing = HeldSendQueue.Sealer(seal: { _ in throw SealFailed() }, open: { $0 })
        XCTAssertFalse(queue(sealer: failing).enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fileURL(conversation).path))
    }

    func testTheFileIsExcludedFromBackup() throws {
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: entry("a")))
        let values = try fileURL(conversation).resourceValues(forKeys: [.isExcludedFromBackupKey])
        XCTAssertEqual(values.isExcludedFromBackup, true)
    }

    func testTheAccountDirectoryNameIsAFileSafeHex() {
        let name = HeldSendQueue.accountDirectoryName("a/b+c=")
        XCTAssertEqual(name, "612f622b633d")
        XCTAssertTrue(HeldSendQueue.isConversationId(name))
    }

    func testAnEmptyOwnerStoresNothing() {
        let q = queue(owner: "")
        XCTAssertFalse(q.enqueue(conversationHex: conversation, entry: entry("a")))
        XCTAssertTrue(q.all().isEmpty)
    }

    // MARK: - the flush report (held-reply-and-flush.test.ts)

    private let sent = HeldFlushReport.Message(localId: "l1", messageId: 6)

    func testTheReportReachesASubscribedViewAndNotAfterItUnsubscribed() {
        let hub = HeldFlushHub()
        var seen: [String] = []
        let off = hub.subscribe { seen.append($0.groupIdB64) }
        hub.notify(HeldFlushReport(groupIdB64: "G", messages: [sent]))
        off()
        off()
        hub.notify(HeldFlushReport(groupIdB64: "H", messages: [sent]))
        XCTAssertEqual(seen, ["G"])
    }

    func testAnEmptyFlushReportsNothingAndOneViewsFailureDoesNotStopTheNext() {
        struct ViewFailed: Error {}
        let hub = HeldFlushHub()
        var seen: [String] = []
        let offA = hub.subscribe { _ in throw ViewFailed() }
        let offB = hub.subscribe { seen.append($0.groupIdB64) }
        hub.notify(HeldFlushReport(groupIdB64: "G", messages: []))
        hub.notify(HeldFlushReport(groupIdB64: "G", messages: [sent]))
        offA()
        offB()
        XCTAssertEqual(seen, ["G"])
    }

    func testTheReportFeedsFlushedRowsToShow() {
        let report = HeldFlushReport(
            groupIdB64: "G",
            messages: [sent, HeldFlushReport.Message(localId: "l2", messageId: 7)])
        let toShow = HeldSend.flushedRowsToShow(
            shownMessageIds: [6], flushed: report.messages, messageId: { $0.messageId })
        XCTAssertEqual(toShow.map(\.localId), ["l2"])
    }

    // MARK: - ADR-0092 — a held content message keeps its content

    func testAHeldContentMessageKeepsItsContentAcrossARelaunchAndIsAMessageWithNoText() throws {
        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: Data(repeating: 3, count: 32))
        let content = try composeContentEnvelope(
            content: .contactCard(card: card), attachments: [], parts: ContentModel.emptyParts)
        let forward = Data("{\"author\":\"Bob\"}".utf8)
        let held = HeldSendQueue.Entry(
            localId: "l1", queuedAtMs: 5, text: "", forward: forward, content: content)
        XCTAssertTrue(held.isValid, "a content message with no text is a message")
        XCTAssertTrue(queue().enqueue(conversationHex: conversation, entry: held))
        let read = try XCTUnwrap(queue().list(conversationHex: conversation).first)
        XCTAssertEqual(read.content, content)
        XCTAssertEqual(read.forward, forward)
        XCTAssertEqual(read.requeued(atMs: 9).content, content, "a requeue keeps the content")
        guard case .contactCard(let back)? = parseBodyEnvelope(bytes: try XCTUnwrap(read.content)).content else {
            return XCTFail("the held bytes still read as the card")
        }
        XCTAssertEqual(back, card)
    }

    func testARowStoredBeforeTheFieldHoldsNoContent() throws {
        let old = Data(#"{"localId":"l1","queuedAtMs":1,"text":"hi","attachments":[]}"#.utf8)
        let decoded = try JSONDecoder().decode(HeldSendQueue.Entry.self, from: old)
        XCTAssertNil(decoded.content)
        XCTAssertTrue(decoded.isValid)
        XCTAssertFalse(HeldSendQueue.Entry(localId: "l2", queuedAtMs: 1, text: "", content: Data()).isValid,
                       "empty content is no content")
    }

    func testOnlyUnsignedContentIsHeldForASuccessor() {
        let location = SharedLocation(latE7: 1, lonE7: 2, accuracyM: 3, fixAtMs: 4, name: nil)
        XCTAssertTrue(HeldSend.holdsContent(.location(location: location)))
        XCTAssertTrue(HeldSend.holdsContent(.contactCard(card: SharedContactCard(
            name: "A", username: "a", accountKey: Data(repeating: 1, count: 32)))))
        let body = ChecklistBody(title: "T", othersAdd: false, othersMark: false, items: [ChecklistItem(id: 1, text: "a")])
        XCTAssertFalse(HeldSend.holdsContent(.checklist(checklist: SignedChecklist(body: body, authorSig: nil))))
    }
}

