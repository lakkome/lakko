// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// A storage server answering from a script, per host.
final class StorageStubURLProtocol: URLProtocol {
    struct Answer {
        let status: Int
        let body: String
    }

    private static let lock = NSLock()
    private static var scripts: [String: [Answer]] = [:]
    private static var seen: [String: [URLRequest]] = [:]

    static func scope(_ answers: [Answer]) -> String {
        let host = "st\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
        lock.withLock { scripts[host] = answers }
        return host
    }

    static func requests(_ host: String) -> [URLRequest] { lock.withLock { seen[host] ?? [] } }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url, let host = url.host?.lowercased() else { return }
        let answer: Answer = Self.lock.withLock {
            Self.seen[host, default: []].append(request)
            guard var queue = Self.scripts[host], !queue.isEmpty else { return Answer(status: 500, body: "{}") }
            let next = queue.removeFirst()
            Self.scripts[host] = queue
            return next
        }
        let response = HTTPURLResponse(
            url: url, statusCode: answer.status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(answer.body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

final class BlobLifetimeTests: XCTestCase {

    private func passBody(_ value: String, expires: Int64 = 4_000_000_000) -> String {
        #"{"pass":"\#(value)","kind":"read","expires_at_unix_secs":\#(expires),"budget":{"downloads":3000,"keeps":5000}}"#
    }

    private func client(_ host: String) -> StorageClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StorageStubURLProtocol.self]
        let session = URLSession(configuration: config)
        let access = BlobAccess(tokens: OfflineAnonTokens(), session: session, baseURL: { "http://\(host)" })
        return StorageClient(
            baseURL: "http://\(host)", session: session, access: access,
            deleteKey: { _ in "delete-key" }, deleteProof: { _, _ in "proof" })
    }

    private let blobId = Data(repeating: 7, count: 32)

    // MARK: - The pass

    func testThePassCacheRefreshesBeforeExpiryAndDropsOnlyTheRefusedPass() {
        var cache = BlobPassCache()
        cache.store(BlobPass(value: "a", kind: .read, expiresAtUnixSecs: 1_000))
        XCTAssertEqual(cache.usable(.read, nowSecs: 900)?.value, "a")
        XCTAssertNil(cache.usable(.read, nowSecs: 941), "a pass is not used in its last minute")
        XCTAssertNil(cache.usable(.upload, nowSecs: 0))
        cache.drop(.read, value: "other")
        XCTAssertEqual(cache.usable(.read, nowSecs: 900)?.value, "a")
        cache.drop(.read, value: "a")
        XCTAssertNil(cache.usable(.read, nowSecs: 900))
    }

    func testRefusalsAreClassified() {
        func body(_ code: String) -> Data { Data(#"{"error":"\#(code)"}"#.utf8) }
        XCTAssertEqual(BlobAnswer.classify(status: 200, body: Data()), .ok)
        XCTAssertEqual(BlobAnswer.classify(status: 404, body: body("blob-not-found")), .gone)
        for code in ["blob-pass-required", "blob-pass-invalid", "blob-pass-expired"] {
            XCTAssertEqual(BlobAnswer.classify(status: 401, body: body(code)), .renewPass)
        }
        XCTAssertEqual(BlobAnswer.classify(status: 429, body: body("blob-read-budget-exhausted")), .renewPass)
        XCTAssertEqual(
            BlobAnswer.classify(status: 503, body: body("blob-node-unavailable")),
            .failed(status: 503, code: "blob-node-unavailable"))
        // nginx's own 429 carries HTML, no code: the generic failure.
        XCTAssertEqual(BlobAnswer.classify(status: 429, body: Data("<html>".utf8)), .failed(status: 429, code: nil))
        XCTAssertTrue(BlobAnswer.isLimit(status: 429, code: "blob-upload-budget-exhausted"))
        XCTAssertFalse(BlobAnswer.isLimit(status: 429, code: nil))
    }

    func testAGetSendsThePassAndAMissingFileIsGone() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("p1")),
            .init(status: 404, body: #"{"error":"blob-not-found"}"#),
        ])
        let data = try await client(host).getBlob(blobId: blobId)
        XCTAssertNil(data)
        let requests = StorageStubURLProtocol.requests(host)
        XCTAssertEqual(requests.map { $0.url?.path }, ["/blob/pass/read", "/blob/\(StorageClient.blobIdHex(blobId))"])
        XCTAssertEqual(requests[0].value(forHTTPHeaderField: AnonTokenRoute.headerName), "offline-blob-read")
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "x-lakko-blob-pass"), "p1")
        XCTAssertNil(requests[1].value(forHTTPHeaderField: "Authorization"))
    }

    func testAnExpiredPassIsRenewedOnceAndASecondBudgetRefusalIsTheLimit() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("p1")),
            .init(status: 401, body: #"{"error":"blob-pass-expired"}"#),
            .init(status: 200, body: passBody("p2")),
            .init(status: 200, body: "cipher"),
        ])
        let data = try await client(host).getBlob(blobId: blobId)
        XCTAssertEqual(data, Data("cipher".utf8))
        XCTAssertEqual(StorageStubURLProtocol.requests(host).last?.value(forHTTPHeaderField: "x-lakko-blob-pass"), "p2")

        let limited = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("q1")),
            .init(status: 429, body: #"{"error":"blob-read-budget-exhausted"}"#),
            .init(status: 200, body: passBody("q2")),
            .init(status: 429, body: #"{"error":"blob-read-budget-exhausted"}"#),
        ])
        do {
            _ = try await client(limited).getBlob(blobId: blobId)
            XCTFail("the limit state")
        } catch StorageClientError.limited(.read) {}
    }

    func testAPutCarriesTheUploadPassAndTheDeleteKey() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("u1")),
            .init(status: 201, body: ""),
        ])
        try await client(host).putBlob(blobId: blobId, ciphertext: Data([1, 2, 3]))
        let put = try XCTUnwrap(StorageStubURLProtocol.requests(host).last)
        XCTAssertEqual(put.httpMethod, "PUT")
        XCTAssertEqual(put.value(forHTTPHeaderField: "x-lakko-blob-pass"), "u1")
        XCTAssertEqual(put.value(forHTTPHeaderField: "x-lakko-blob-delete-key"), "delete-key")
        XCTAssertEqual(StorageStubURLProtocol.requests(host).first?.url?.path, "/blob/pass/upload")
    }

    func testADeleteIsSignedAndARefusalCountsAsDone() async throws {
        let host = StorageStubURLProtocol.scope([.init(status: 403, body: #"{"error":"blob-delete-refused"}"#)])
        try await client(host).deleteBlob(blobId: blobId)
        let request = try XCTUnwrap(StorageStubURLProtocol.requests(host).first)
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.value(forHTTPHeaderField: storageProofHeaderName()), "proof")
        XCTAssertNil(request.value(forHTTPHeaderField: "x-lakko-blob-pass"))
    }

    // MARK: - The media states

    func testMediaFailuresAreGoneLimitedOrGeneric() {
        XCTAssertEqual(MediaLoadFailure.of(AttachmentBridgeError.missing(blobId: Data())), .gone)
        XCTAssertEqual(MediaLoadFailure.of(AttachmentBridgeError.readLimited), .limited)
        XCTAssertEqual(MediaLoadFailure.of(StorageClientError.limited(.read)), .limited)
        XCTAssertEqual(MediaLoadFailure.of(AttachmentBridgeError.download(URLError(.timedOut))), .failed)
        XCTAssertFalse(MediaLoadFailure.gone.retries)
        XCTAssertTrue(MediaLoadFailure.limited.retries)
        XCTAssertEqual(
            AttachmentBridgeError.uploadLimited.userText(fileName: nil).text,
            Localization.text("chat.attachment.limit.send"))
        XCTAssertEqual(
            AttachmentBridgeError.missing(blobId: Data()).userText(fileName: nil).text,
            Localization.text("chat.attachment.gone.title"))
    }

    // MARK: - Delete list and keep schedule

    func testTheDeleteListDeduplicatesAndIsBounded() {
        var list = BlobDeleteList()
        list.add([blobId, blobId, Data(count: 3)])
        XCTAssertEqual(list.hexIds, [StorageClient.blobIdHex(blobId)])
        list.remove(StorageClient.blobIdHex(blobId))
        XCTAssertTrue(list.isEmpty)
        list.add((0..<(BlobDeleteList.maxEntries + 5)).map { i in
            var d = Data(repeating: 0, count: 32)
            d[0] = UInt8(i & 0xFF); d[1] = UInt8((i >> 8) & 0xFF)
            return d
        })
        XCTAssertEqual(list.hexIds.count, BlobDeleteList.maxEntries)
    }

    func testTheDeleteListSurvivesInItsStore() throws {
        let suite = "lakko.tests.blob-lifecycle.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = BlobLifecycleStore(defaults: defaults)
        var list = BlobDeleteList()
        list.add([blobId])
        store.setDeletes(list, accountIdB64: "acct")
        XCTAssertEqual(store.deletes(accountIdB64: "acct"), list)
        XCTAssertTrue(store.deletes(accountIdB64: "other").isEmpty)
        store.clear(accountIdB64: "acct")
        XCTAssertTrue(store.deletes(accountIdB64: "acct").isEmpty)
    }

    func testKeepsAreSpreadByAHashOfTheKeyAndSentAboutMonthly() {
        let month = KeepSchedule.intervalMs
        // The web rule: FNV-1a (32-bit) of the key, modulo the interval.
        XCTAssertEqual(KeepSchedule.spreadOffsetMs("saved"), 1_203_033_924)
        var schedule = KeepSchedule()
        let now = 10 * month
        schedule.adopt(["saved"], nowMs: now)
        XCTAssertEqual(schedule.due(["saved"], nowMs: now + 1_203_033_923), [])
        XCTAssertEqual(schedule.due(["saved"], nowMs: now + 1_203_033_924), ["saved"])
        schedule.sent("saved", nowMs: now + 1_203_033_924)
        XCTAssertEqual(schedule.due(["saved"], nowMs: now + 1_203_033_924 + month - 1), [])
        schedule.retain([])
        XCTAssertEqual(schedule.due(["saved"], nowMs: 100 * month), [], "a forgotten set is adopted again")
    }

    func testATickTakesAtMostFourDueSetsMostOverdueFirst() {
        let keys = (0..<7).map { "pins:\($0)" }
        var schedule = KeepSchedule()
        schedule.adopt(keys, nowMs: 0)
        let due = schedule.due(keys, nowMs: 2 * KeepSchedule.intervalMs)
        XCTAssertEqual(due.count, 4)
        let order = keys.sorted {
            let a = KeepSchedule.spreadOffsetMs($0), b = KeepSchedule.spreadOffsetMs($1)
            return a < b || (a == b && $0 < $1)
        }
        XCTAssertEqual(due, Array(order.prefix(4)))
    }

    func testADeleteThatIsNotARefusalStaysOwed() async {
        let host = StorageStubURLProtocol.scope([.init(status: 404, body: "")])
        do {
            try await client(host).deleteBlob(blobId: blobId)
            XCTFail("a 404 from DELETE is retried later")
        } catch {}
    }

    // MARK: - The delete key across a rotation (ADR-0074)

    func testTheDeleteKeyIsTheSameBeforeAndAfterARotation() throws {
        let old = Data(repeating: 0x11, count: 64)
        let rotated = Data(repeating: 0x22, count: 64)
        let store = InMemoryOwnPredecessorStore()
        let before = try XCTUnwrap(BlobDeleteKeys.seed(accountIdB64: "acct", currentSeed: old, predecessors: store))
        XCTAssertEqual(before, old, "no predecessor: the current seed")
        // The rotation keeps the old seed as the oldest predecessor.
        try store.add(old, accountIdB64: "acct")
        let after = try XCTUnwrap(BlobDeleteKeys.seed(accountIdB64: "acct", currentSeed: rotated, predecessors: store))
        let keyBefore = try XCTUnwrap(BlobDeleteKeys.publicKey(seed: before, blobId: blobId))
        let keyAfter = try XCTUnwrap(BlobDeleteKeys.publicKey(seed: after, blobId: blobId))
        XCTAssertEqual(keyBefore, keyAfter)
        XCTAssertNotEqual(BlobDeleteKeys.publicKey(seed: rotated, blobId: blobId), keyBefore)
    }

    func testASavedRowShowsTheMessagesDate() {
        let message = ChatMessage(
            messageId: 1, direction: .incoming, text: "x", createdAt: "2026-10-26T10:00:00Z")
        XCTAssertEqual(SavedMessages.sentAtMs(message), UInt64(DsTimestamp.parseMs("2026-10-26T10:00:00Z") ?? -1))
        XCTAssertEqual(SavedMessages.sentAtMs(ChatMessage(messageId: 1, direction: .incoming, text: "", createdAt: "?")), 0)
        XCTAssertEqual(SavedMessages.rowDateMs(snapshotSentAtMs: 5, heldSentAtMs: 7, savedAtMs: 9), 5)
        XCTAssertEqual(SavedMessages.rowDateMs(snapshotSentAtMs: 0, heldSentAtMs: 7, savedAtMs: 9), 7)
        XCTAssertEqual(SavedMessages.rowDateMs(snapshotSentAtMs: 0, heldSentAtMs: nil, savedAtMs: 9), 9)
        XCTAssertEqual(SavedMessages.rowDateMs(snapshotSentAtMs: 0, heldSentAtMs: 0, savedAtMs: 9), 9)
    }

    func testKeepSelectionTakesPinnedSavedAndProfileFiles() {
        let thumb = ThumbRef(
            blobId: Data(repeating: 2, count: 32), key: Data(count: 32), nonce: Data(count: 12),
            mime: "image/jpeg", size: 1, width: nil, height: nil)
        let ref = AttachmentRef(
            blobId: Data(repeating: 1, count: 32), key: Data(count: 32), nonce: Data(count: 12),
            mime: "image/jpeg", size: 10, name: nil, kind: nil, durationMs: nil, waveform: nil,
            width: nil, height: nil, thumb: thumb)
        let pinned = ChatMessage(messageId: 5, direction: .incoming, text: "", attachments: [ref], createdAt: "")
        let other = ChatMessage(messageId: 6, direction: .incoming, text: "", attachments: [ref], createdAt: "")
        let thread = ChatThread(groupIdHex: "aa", messages: [pinned, other], cursor: 0)
        let sets = KeepSelection.sets(
            threads: [thread], pinnedIds: { _ in [5] },
            savedBlobIds: [Data(repeating: 9, count: 32)], profileBlobIds: [])
        XCTAssertEqual(sets["pins:aa"], [ref.blobId, thumb.blobId])
        XCTAssertEqual(KeepSelection.pinsKey("AB"), "pins:ab")
        XCTAssertEqual(sets[KeepSelection.savedKey], [Data(repeating: 9, count: 32)])
        XCTAssertNil(sets[KeepSelection.profileKey])
        XCTAssertEqual(MessageBlobs.ids(of: [ref, ref]), [ref.blobId, thumb.blobId])
    }

    // MARK: - Saved

    private func view(_ id: UInt64, savedAt: UInt64, hlc: UInt64 = 1, kind: String = "", excerpt: String = "x") -> SavedMessageView {
        SavedMessageView(
            hlc: hlc, conversation: FolderConversationRef(kind: .group, id: Data(repeating: 0xAA, count: 1)),
            messageId: id, savedAtMs: savedAt, author: "Ann", excerpt: excerpt, attachmentKind: kind, blobIds: [])
    }

    func testSavedEntriesAreMostRecentFirstAndMatchedByConversationId() {
        let list = SavedMessages.ordered([view(1, savedAt: 10), view(2, savedAt: 30), view(3, savedAt: 30, hlc: 9)])
        XCTAssertEqual(list.map(\.messageId), [3, 2, 1])
        XCTAssertNotNil(SavedMessages.entry(in: list, ref: SavedMessageRef(segmentIdHex: "aa", dsId: 2)))
        XCTAssertNil(SavedMessages.entry(in: list, ref: SavedMessageRef(segmentIdHex: "bb", dsId: 2)))
        XCTAssertNil(SavedMessages.entry(in: list, ref: SavedMessageRef(segmentIdHex: "aa", dsId: 0)))
    }

    func testSnapshotKindAndRowLine() {
        let file = AttachmentRef(
            blobId: Data(count: 32), key: Data(count: 32), nonce: Data(count: 12),
            mime: "application/pdf", size: 1, name: "a.pdf")
        XCTAssertEqual(SavedMessages.kindToken([]), "")
        XCTAssertEqual(SavedMessages.kindToken([file]), "file")
        let snap = SavedMessages.snapshot(text: "", attachments: [file], author: "Ann")
        XCTAssertEqual(snap.excerpt, "a.pdf")
        XCTAssertEqual(
            SavedMessages.rowLine(excerpt: "a.pdf", kind: "file"),
            Localization.text("saved.kind.file", texts: ["name": "a.pdf"]))
        XCTAssertEqual(SavedMessages.rowLine(excerpt: "", kind: "photo"), Localization.text("saved.kind.photo"))
        XCTAssertEqual(SavedMessages.rowLine(excerpt: "hello", kind: ""), "hello")
        XCTAssertTrue(SavedMessages.canSaveMore(currentCount: Int(savedMaxClient()) - 1))
        XCTAssertFalse(SavedMessages.canSaveMore(currentCount: Int(savedMaxClient())))
    }

    func testADeletedMessageLeavesSavedAndRowsComeFromTheSnapshotUntilHeld() {
        var deleted = ChatMessage(messageId: 1, direction: .incoming, text: "gone", attachments: [], createdAt: "")
        deleted.deleted = true
        let held = ChatMessage(messageId: 2, direction: .incoming, text: "fresh", attachments: [], createdAt: "")
        let thread = ChatThread(groupIdHex: "aa", messages: [deleted, held], cursor: 0)
        let entries = [view(1, savedAt: 1), view(2, savedAt: 2), view(3, savedAt: 3, excerpt: "snap")]
        XCTAssertEqual(SavedMessagesCenter.deletedEntries(entries, threads: [thread]).map(\.messageId), [1])

        let rows = SavedRow.rows(entries, threads: [thread], title: { _ in "Group" })
        XCTAssertEqual(rows.map(\.ref.dsId), [3, 2, 1])
        XCTAssertEqual(rows[0].line, "snap")
        XCTAssertFalse(rows[0].held)
        XCTAssertEqual(rows[1].line, "fresh")
        XCTAssertTrue(rows[1].held)
    }

    // MARK: - Saving across conversation segments (round 6)

    private func segmentView(_ segment: UInt8, _ id: UInt64) -> SavedMessageView {
        SavedMessageView(
            hlc: 1, conversation: FolderConversationRef(kind: .group, id: Data(repeating: segment, count: 32)),
            messageId: id, savedAtMs: 1, author: "Ann", excerpt: "snap", attachmentKind: "", blobIds: [])
    }

    private func hex(_ byte: UInt8) -> String { ChatFolders.hex(Data(repeating: byte, count: 32)) }

    func testAMessageSavedBeforeItsConversationMovesIsFoundByTheSameRegisterAfter() {
        let message = ChatMessage(messageId: 9, direction: .incoming, text: "hi", createdAt: "")
        // Before: the message lives in the current group A.
        let before = ChatThread(groupIdHex: hex(0xA1), messages: [message], cursor: 0)
        let ref = before.savedRef(rowId: 9)
        XCTAssertEqual(ref, SavedMessageRef(segmentIdHex: hex(0xA1), dsId: 9))
        // After: the conversation continues in B; A's rows are history.
        let historyId = LineageRules.historyRowId(9, distanceFromHead: 1)
        let moved = ChatMessage(messageId: historyId, direction: .incoming, text: "hi", createdAt: "")
        let after = ChatThread(
            groupIdHex: hex(0xB2), messages: [moved], cursor: 0,
            conversationIdHex: hex(0xA1), historyGroupIdHexes: [hex(0xA1)], segmentIndex: 1)
        XCTAssertEqual(after.savedRef(rowId: historyId), ref, "the same register names it")
        XCTAssertEqual(after.rowId(of: ref!), historyId)

        let rows = SavedRow.rows([segmentView(0xA1, 9)], threads: [after], title: { $0 == self.hex(0xB2) ? "Team" : nil })
        XCTAssertEqual(rows.first?.openGroupIdHex, hex(0xB2))
        XCTAssertEqual(rows.first?.rowId, historyId)
        XCTAssertEqual(rows.first?.chatTitle, "Team", "the title is the holding conversation's")
        XCTAssertTrue(rows.first?.held ?? false)
    }

    func testAnUnheldSegmentTakesItsTitleThroughTheLineageAndDoesNotOpen() {
        let head = ChatThread(groupIdHex: hex(0xB2), messages: [], cursor: 0)
        let rows = SavedRow.rows(
            [segmentView(0xA1, 9)], threads: [head],
            headOf: { $0 == self.hex(0xA1) ? self.hex(0xB2) : $0 },
            title: { $0 == self.hex(0xB2) ? "Team" : nil })
        XCTAssertEqual(rows.first?.chatTitle, "Team")
        XCTAssertFalse(rows.first?.held ?? true)
        XCTAssertEqual(rows.first?.line, "snap")
    }

    func testDeleteForEveryoneOnAnOlderSegmentRowMatchesBySegment() {
        let historyId = LineageRules.historyRowId(9, distanceFromHead: 1)
        let gone = ChatMessage(messageId: historyId, direction: .incoming, text: "", createdAt: "", deleted: true)
        let thread = ChatThread(
            groupIdHex: hex(0xB2), messages: [gone], cursor: 0, historyGroupIdHexes: [hex(0xA1)])
        let entries = [segmentView(0xA1, 9), segmentView(0xB2, 9)]
        XCTAssertEqual(
            SavedMessagesCenter.deletedEntries(entries, threads: [thread]).map { SavedMessages.ref(of: $0) },
            [SavedMessageRef(segmentIdHex: hex(0xA1), dsId: 9)],
            "only the older segment's message 9 went, not the head's message 9")
        XCTAssertNil(thread.savedRef(rowId: LineageRules.historyRowId(9, distanceFromHead: 2)), "no such segment held")
    }

    func testSaveSitsDirectlyUnderPin() {
        let actions = MessageMenuAction.offered(
            canReply: true, canForward: true, canEditOrDelete: true, hasText: true,
            pin: .pin, canSelect: true, save: .save)
        XCTAssertEqual(actions, [.reply, .forward, .copy, .select, .pin, .save, .edit, .delete])
        XCTAssertEqual(SaveMenuState.of(isSaved: true, available: true), .unsave)
        XCTAssertEqual(SaveMenuState.of(isSaved: false, available: false), .unavailable)
    }

    func testAReplacedProfilePhotoDeletesOnlyWhatIsNoLongerUsed() {
        let a = AvatarBlobRef(blobId: Data(repeating: 1, count: 32), key: Data(count: 32), nonce: Data(count: 12), size: 1, side: 1)
        let b = AvatarBlobRef(blobId: Data(repeating: 2, count: 32), key: Data(count: 32), nonce: Data(count: 12), size: 1, side: 1)
        XCTAssertEqual(
            ProfileAvatarUpload.replacedBlobs(previous: ProfileAvatar(still: a), current: ProfileAvatar(still: b)),
            [a.blobId])
        XCTAssertEqual(ProfileAvatarUpload.replacedBlobs(previous: ProfileAvatar(still: a), current: nil), [a.blobId])
        XCTAssertEqual(ProfileAvatarUpload.replacedBlobs(previous: ProfileAvatar(still: a), current: ProfileAvatar(still: a)), [])
    }
}
