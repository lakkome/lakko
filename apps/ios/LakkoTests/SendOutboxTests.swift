// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/connection-state.md` section 7.3 — the account's outbox on disk.
/// Every case runs against the real store and the real device-key wrap (the
/// Keychain key the message log uses), in a directory of its own, except the
/// wipe case, which runs the real wipe over the app's own directory.
@MainActor
final class SendOutboxTests: XCTestCase {

    private var base: URL!
    private var owner = ""
    private let conversation = String(repeating: "ab", count: 32)

    private static let deviceKey = HeldSendQueue.Sealer(
        seal: { try MessageLogStore.sealUnderWrapKey($0) },
        open: { try MessageLogStore.openUnderWrapKey($0) })

    override func setUpWithError() throws {
        try super.setUpWithError()
        base = FileManager.default.temporaryDirectory
            .appendingPathComponent("send-outbox-tests-\(UUID().uuidString)", isDirectory: true)
        owner = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
    }

    override func tearDownWithError() throws {
        if let base { try? FileManager.default.removeItem(at: base) }
        base = nil
        try super.tearDownWithError()
    }

    private func outbox(baseDirectory: URL? = nil) -> SendOutbox {
        SendOutbox(ownerAccountId: owner, sealer: Self.deviceKey, baseDirectory: baseDirectory ?? base)
    }

    private func entry(_ localId: Int64, _ text: String, media: SendOutbox.PreparedMedia? = nil) -> SendOutbox.Entry {
        SendOutbox.Entry(
            localId: localId, writtenAtMs: 1_000 - localId, state: .waiting, autoSendAttempts: 2,
            row: PersistedMessage(
                messageId: 0, direction: "outgoing", text: text, attachments: [],
                createdAt: "2026-11-24T10:00:00Z",
                reply: PersistedReply(target: 7, author: "Ann", excerpt: "hello")),
            media: media.map { [$0] } ?? [])
    }

    func testEntriesRoundTripSealedAndInOrder() throws {
        let store = outbox()
        let media = SendOutbox.PreparedMedia(
            fileId: SendOutbox.newFileId(), name: "voice.m4a", mime: "audio/mp4", kind: "voice",
            durationMs: 1_500, waveform: Data([1, 2, 3]))
        XCTAssertTrue(store.save(conversationHex: conversation, entries: [entry(-1, "one"), entry(-2, "", media: media)]))
        XCTAssertEqual(store.list(conversationHex: conversation), [entry(-1, "one"), entry(-2, "", media: media)])
        // What is on disk is not the plaintext.
        let file = base.appendingPathComponent("SendOutbox", isDirectory: true)
            .appendingPathComponent(HeldSendQueue.accountDirectoryName(owner), isDirectory: true)
            .appendingPathComponent(conversation).appendingPathExtension("outbox")
        let raw = try Data(contentsOf: file)
        XCTAssertNil(raw.range(of: Data("voice.m4a".utf8)))
        XCTAssertEqual(store.all().map(\.conversationHex), [conversation])
        // An empty list removes the file.
        XCTAssertTrue(store.save(conversationHex: conversation, entries: []))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        XCTAssertTrue(store.all().isEmpty)
    }

    func testAPreparedFileIsSealedAndOrphansGo() throws {
        let store = outbox()
        let kept = SendOutbox.newFileId()
        let orphan = SendOutbox.newFileId()
        let bytes = Data("recorded audio".utf8)
        XCTAssertTrue(store.writeFile(id: kept, bytes: bytes))
        XCTAssertTrue(store.writeFile(id: orphan, bytes: Data([9])))
        XCTAssertEqual(store.readFile(id: kept), bytes)
        store.save(conversationHex: conversation, entries: [entry(-1, "", media: SendOutbox.PreparedMedia(
            fileId: kept, name: "a.m4a", mime: "audio/mp4", kind: "voice"))])
        store.removeOrphanFiles()
        XCTAssertTrue(store.hasFile(id: kept))
        XCTAssertFalse(store.hasFile(id: orphan))
        XCTAssertTrue(store.removeFile(id: kept))
        XCTAssertNil(store.readFile(id: kept))
    }

    func testAFileThatDoesNotOpenReadsAsEmpty() {
        let store = outbox()
        store.save(conversationHex: conversation, entries: [entry(-1, "one")])
        let other = SendOutbox(
            ownerAccountId: owner,
            sealer: HeldSendQueue.Sealer(seal: { $0 }, open: { _ in throw URLError(.cannotDecodeContentData) }),
            baseDirectory: base)
        XCTAssertTrue(other.list(conversationHex: conversation).isEmpty)
    }

    /// Section 7.3 "Wipe" — the account's wipe takes the outbox and its
    /// prepared files with the rest of its local data, and nothing is
    /// written back afterwards (ADR-0077).
    func testTheAccountWipeClearsTheOutboxAndRefusesLaterWrites() {
        let app = SendOutbox(ownerAccountId: owner, sealer: Self.deviceKey)
        let fileId = SendOutbox.newFileId()
        XCTAssertTrue(app.writeFile(id: fileId, bytes: Data([1, 2])))
        XCTAssertTrue(app.save(conversationHex: conversation, entries: [entry(-1, "kept")]))
        AccountLocalDataWipe.wipe(accountIdB64: owner, removeDeliveredNotifications: {})
        XCTAssertTrue(app.list(conversationHex: conversation).isEmpty)
        XCTAssertFalse(app.hasFile(id: fileId))
        XCTAssertFalse(app.save(conversationHex: conversation, entries: [entry(-1, "late")]))
        XCTAssertTrue(app.list(conversationHex: conversation).isEmpty)
    }

    /// The entries a conversation's rows come to: unsent outgoing rows only,
    /// sending counted as waiting, held rows left to the held queue.
    func testTheEntriesOfAConversationAreItsUnsentOwnRows() {
        var waiting = ChatMessage(
            messageId: 0, direction: .outgoing, text: "w", createdAt: "2026-11-24T10:00:01Z",
            ack: .waiting, localId: -1)
        waiting.autoSendAttempts = 3
        let failed = ChatMessage(
            messageId: 0, direction: .outgoing, text: "f", createdAt: "2026-11-24T10:00:02Z",
            ack: .failed, localId: -2)
        let sending = ChatMessage(
            messageId: 0, direction: .outgoing, text: "s", createdAt: "2026-11-24T10:00:03Z",
            ack: .pending, localId: -3)
        let held = ChatMessage(
            messageId: 0, direction: .outgoing, text: "h", createdAt: "2026-11-24T10:00:04Z",
            ack: .pending, localId: -4)
        let sent = ChatMessage(
            messageId: 9, direction: .outgoing, text: "x", createdAt: "2026-11-24T10:00:05Z", localId: -5)
        let incoming = ChatMessage(messageId: 10, direction: .incoming, text: "i", createdAt: "2026-11-24T10:00:06Z")
        let entries = ChatRepository.outboxEntries(
            rows: [waiting, failed, sending, held, sent, incoming], heldLocalIds: [-4])
        XCTAssertEqual(entries.map(\.localId), [-1, -2, -3])
        XCTAssertEqual(entries.map(\.state), [.waiting, .notSent, .waiting])
        XCTAssertEqual(entries.first?.autoSendAttempts, 3)
        XCTAssertLessThan(entries[0].writtenAtMs, entries[1].writtenAtMs)
        let restored = ChatRepository.restoredRow(entries[1])
        XCTAssertEqual(restored?.ack, .failed)
        XCTAssertEqual(restored?.localId, -2)
        XCTAssertEqual(restored?.text, "f")
    }
}
