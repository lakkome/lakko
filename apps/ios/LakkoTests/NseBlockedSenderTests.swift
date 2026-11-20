// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import XCTest
// The extension's decrypt core (`NsePushDecrypt.swift`) is compiled straight
// into this target; the mirror and the channel report source come from the
// app module.
@testable import Lakko
import LakkoCore

/// ADR-0098 section 6 — the notification extension honours a block: the app
/// mirrors the blocked account keys into the App Group, and the extension
/// shows nothing for a row whose MLS-resolved sender is in that set. Also the
/// channel info's report, which offers the posts of the local post log.
final class NseBlockedSenderTests: XCTestCase {

    private let blockedKey = Data(repeating: 0xBB, count: 32)
    private let friendKey = Data(repeating: 0xCC, count: 32)
    private var directory: URL!
    private let wrapKey = SymmetricKey(data: Data(repeating: 0x42, count: 32))
    private var store: BlockedSenderMirror.Store!

    override func setUp() {
        super.setUp()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("nse-blocked-sender-tests-\(UUID().uuidString)")
        let dir = directory!
        let key = wrapKey
        store = BlockedSenderMirror.Store(directory: { dir }, sealKey: { key }, openKey: { key })
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private func files() -> [URL] {
        (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
    }

    // MARK: - The mirror

    func testTheMirrorWritesReadsAndClearsPerAccount() {
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "acct", store: store), [])
        BlockedSenderMirror.replace(
            accountIdB64: "acct", accountKeys: [friendKey, blockedKey], store: store)
        let read = BlockedSenderMirror.read(accountIdB64: "acct", store: store)
        XCTAssertEqual(read, [friendKey, blockedKey].map { $0.base64EncodedString() }.sorted())
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "other", store: store), [],
                       "one file per account")
        BlockedSenderMirror.replace(accountIdB64: "acct", accountKeys: [], store: store)
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "acct", store: store), [],
                       "an unblock leaves the mirror")
        BlockedSenderMirror.replace(accountIdB64: "acct", accountKeys: [blockedKey], store: store)
        BlockedSenderMirror.clear(accountIdB64: "acct", store: store)
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "acct", store: store), [])
        XCTAssertEqual(files(), [])
    }

    func testANilAccountWritesNothing() {
        BlockedSenderMirror.replace(accountIdB64: nil, accountKeys: [blockedKey], store: store)
        XCTAssertEqual(files(), [])
    }

    /// F9 — the set is sealed: the file names no account and holds no key in
    /// the clear, and it is excluded from backup.
    func testTheMirrorIsSealedAndExcludedFromBackup() throws {
        let accountIdB64 = "acct/with+chars=="
        BlockedSenderMirror.replace(accountIdB64: accountIdB64, accountKeys: [blockedKey], store: store)
        let written = files()
        XCTAssertEqual(written.count, 1)
        let url = try XCTUnwrap(written.first)
        XCTAssertFalse(url.lastPathComponent.contains("acct"), "the file name is a hash")
        let bytes = try Data(contentsOf: url)
        for needle in [blockedKey.base64EncodedString(), blockedKey.base64EncodedString().prefix(16).description] {
            XCTAssertNil(bytes.range(of: Data(needle.utf8)), "no blocked key in the clear")
        }
        XCTAssertNil(bytes.range(of: blockedKey), "no raw blocked key")
        let values = try url.resourceValues(forKeys: [.isExcludedFromBackupKey])
        XCTAssertEqual(values.isExcludedFromBackup, true)
        let dirValues = try directory.resourceValues(forKeys: [.isExcludedFromBackupKey])
        XCTAssertEqual(dirValues.isExcludedFromBackup, true)
        XCTAssertEqual(
            BlockedSenderMirror.read(accountIdB64: accountIdB64, store: store),
            [blockedKey.base64EncodedString()])
    }

    func testAnotherKeyOrAnotherAccountOpensNothing() throws {
        BlockedSenderMirror.replace(accountIdB64: "acct", accountKeys: [blockedKey], store: store)
        let dir = directory!
        let wrong = SymmetricKey(data: Data(repeating: 0x43, count: 32))
        let other = BlockedSenderMirror.Store(directory: { dir }, sealKey: { wrong }, openKey: { wrong })
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "acct", store: other), [],
                       "a file sealed under another key reads as none")
        let noKey = BlockedSenderMirror.Store(directory: { dir }, sealKey: { nil }, openKey: { nil })
        XCTAssertEqual(BlockedSenderMirror.read(accountIdB64: "acct", store: noKey), [],
                       "no key reads as none (the extension never creates one)")

        // One account's file copied over another's does not open: the
        // account id is the associated data.
        BlockedSenderMirror.replace(accountIdB64: "acct2", accountKeys: [friendKey], store: store)
        let all = files()
        XCTAssertEqual(all.count, 2)
        let a = try Data(contentsOf: all[0])
        try a.write(to: all[1])
        let reads = ["acct", "acct2"].map { BlockedSenderMirror.read(accountIdB64: $0, store: store) }
        XCTAssertTrue(reads.contains([]), "the swapped file opens as none")
    }

    func testTheDecision() {
        let set = [blockedKey.base64EncodedString()]
        XCTAssertTrue(BlockedSenderMirror.dropsNotification(senderKeyB64: blockedKey.base64EncodedString(), blockedKeysB64: set))
        XCTAssertFalse(BlockedSenderMirror.dropsNotification(senderKeyB64: friendKey.base64EncodedString(), blockedKeysB64: set))
        XCTAssertFalse(BlockedSenderMirror.dropsNotification(senderKeyB64: nil, blockedKeysB64: set),
                       "an unresolved sender is never treated as blocked")
        XCTAssertFalse(BlockedSenderMirror.dropsNotification(senderKeyB64: "", blockedKeysB64: [""]))
    }

    // MARK: - The extension's silent drop

    private struct Deps: NsePushDecryptDeps {
        let blocked: [String]
        let rows: [NseFetchedRow]
        let group: NseThrowawayGroup?

        func personName(accountIdB64: String, keyB64: String) -> String? { nil }
        func reactionNotificationsOn() -> Bool { true }
        func boundAccount(wakeSubscriptionIdHex: String?) -> String? { "acct" }
        func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? {
            pushGidHex
        }
        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool { true }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(seed: Data, snapshot: Data, groupIdBytes: Data) throws -> NseThrowawayGroup? {
            group
        }
        func dsFetch(groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup) async throws -> [NseFetchedRow] {
            rows
        }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
        func isBlockedSender(accountIdB64: String, senderKeyB64: String?) -> Bool {
            BlockedSenderMirror.dropsNotification(senderKeyB64: senderKeyB64, blockedKeysB64: blocked)
        }
    }

    private final class Group: NseThrowawayGroup {
        let decode: (Data) -> NseProcessedRow?
        init(_ decode: @escaping (Data) -> NseProcessedRow?) { self.decode = decode }
        func process(_ payload: Data) -> NseProcessedRow? { decode(payload) }
        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() {}
    }

    private func chat(from key: Data) -> NseProcessedRow {
        NseProcessedRow(kind: .chat, text: "hello", attachmentCount: 0, senderKeyB64: key.base64EncodedString())
    }

    func testARowFromABlockedSenderDropsSilentlyWhateverItsKind() {
        let deps = Deps(blocked: [blockedKey.base64EncodedString()], rows: [], group: nil)
        XCTAssertTrue(nseDropsSilently(chat(from: blockedKey), accountIdB64: "acct", receivedGroupIdHex: "aa", deps: deps))
        let reaction = NseProcessedRow(
            kind: .reaction, text: "", attachmentCount: 0, senderKeyB64: blockedKey.base64EncodedString())
        XCTAssertTrue(nseDropsSilently(reaction, accountIdB64: "acct", receivedGroupIdHex: "aa", deps: deps))
        XCTAssertFalse(nseDropsSilently(chat(from: friendKey), accountIdB64: "acct", receivedGroupIdHex: "aa", deps: deps))
    }

    func testAWakeCarryingOnlyABlockedSendersMessageShowsNoPreview() async {
        let groupHex = String(repeating: "a", count: 64)
        let blocked = Deps(
            blocked: [blockedKey.base64EncodedString()],
            rows: [NseFetchedRow(id: 5, payload: Data([1]))],
            group: Group { _ in self.chat(from: self.blockedKey) })
        let outcome = await decryptPushOutcome(NsePushWake(groupHex: groupHex, pointer: 5), blocked)
        if case .preview = outcome { XCTFail("a blocked person's message is never previewed") }

        let unblocked = Deps(
            blocked: [],
            rows: [NseFetchedRow(id: 5, payload: Data([1]))],
            group: Group { _ in self.chat(from: self.blockedKey) })
        let shown = await decryptPushOutcome(NsePushWake(groupHex: groupHex, pointer: 5), unblocked)
        guard case .preview = shown else { return XCTFail("the same row from someone not blocked is previewed") }
    }

    // MARK: - Channel info's report

    func testAChannelReportOffersTheLoggedPostsWithTheirWires() throws {
        let channelId = Data(repeating: 0x0C, count: 16)
        let logged = [
            PersistedChannelPost(cursor: 3, serverTsMs: 1, text: "third", payloadB64: base64UrlEncodeNoPad(Data([3]))),
            PersistedChannelPost(cursor: 1, serverTsMs: 1, text: "first"),
        ]
        let loaded = [PostView(cursor: 1, serverTsMs: 1, payloadB64: base64UrlEncodeNoPad(Data([1])))]
        let request = ChannelReportSource.request(channelId: channelId, title: "News", logged: logged, loaded: loaded)
        XCTAssertEqual(request.candidates.map(\.messageId), [3, 1], "newest first")
        XCTAssertEqual(request.followUp, .unsubscribe)
        let evidence = try XCTUnwrap(request.candidateEvidence)
        guard case .channelPost(let id, let post) = evidence(3) else { return XCTFail("a logged wire") }
        XCTAssertEqual(id, channelId)
        XCTAssertEqual(post, Data([3]))
        guard case .channelPost(_, let fallback) = evidence(1) else {
            return XCTFail("a post logged without its wire falls back to the loaded one")
        }
        XCTAssertEqual(fallback, Data([1]))
        XCTAssertNil(evidence(9))
    }
}
