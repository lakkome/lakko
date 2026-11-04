// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0087 phase A on iOS: the reaction fold wiring, the group-setting gate,
/// the `:` query and the bounded avatar store.
final class GroupSettingsAndReactionsTests: XCTestCase {

    private let anna = Data(repeating: 1, count: 32).base64EncodedString()
    private let ivan = Data(repeating: 2, count: 32).base64EncodedString()

    // MARK: - Reaction fold (reactions-and-emoji spec section 10)

    func testAFourthReactionOfOnePersonIsIgnoredForGood() {
        let ops = [
            Reaction(reactor: anna, emoji: "👍", add: true, at: 1),
            Reaction(reactor: anna, emoji: "❤️", add: true, at: 2),
            Reaction(reactor: anna, emoji: "😂", add: true, at: 3),
            Reaction(reactor: anna, emoji: "😮", add: true, at: 4),
            Reaction(reactor: anna, emoji: "👍", add: false, at: 5),
        ]
        let held = ReactionFold.held(ops: ops).map(\.emoji)
        XCTAssertEqual(Set(held), ["❤️", "😂"])
    }

    func testAnAddOfAHeldEmojiAndARemoveOfAnAbsentOneAreNoOps() {
        let ops = [
            Reaction(reactor: anna, emoji: "👍", add: true, at: 1),
            Reaction(reactor: anna, emoji: "👍", add: true, at: 2),
            Reaction(reactor: ivan, emoji: "❤️", add: false, at: 3),
            Reaction(reactor: ivan, emoji: "👍", add: true, at: 4),
        ]
        let message = ChatMessage(
            messageId: 9, direction: .outgoing, text: "hi", createdAt: "", reactions: ops)
        XCTAssertEqual(message.heldReactions.count, 2)
        XCTAssertEqual(message.heldEmoji(of: anna), ["👍"])
        XCTAssertEqual(message.heldKinds, ["👍"])
    }

    func testTheOwnSentinelStillShowsItsTap() {
        let ops = [Reaction(reactor: AuthorIdentity.ownReactionSentinel, emoji: "🙏", add: true, at: 0)]
        XCTAssertEqual(ReactionFold.held(ops: ops).first?.reactor, AuthorIdentity.ownReactionSentinel)
    }

    // MARK: - Group setting gate (ADR-0087 section 18.1)

    func testOnlyTheOwnerOrAnAdminMaySetAGroupSetting() {
        let owner = Data(repeating: 7, count: 32)
        let admin = Data(repeating: 8, count: 32)
        let member = Data(repeating: 9, count: 32)
        let authority = PushGroupRouting.SecretAuthority.roleTable(owner: owner, admins: [admin])
        func verdict(_ sender: Data?, stored: UInt64? = nil, incoming: UInt64 = 10) -> GroupNameVerdict {
            GroupSettingGate.classify(
                identical: false, incomingUpdatedAtMs: incoming, storedUpdatedAtMs: stored,
                senderKey: sender, authority: authority, isNewer: { $0 > $1 })
        }
        XCTAssertEqual(verdict(owner), .accept)
        XCTAssertEqual(verdict(admin), .accept)
        XCTAssertEqual(verdict(member), .rejectNotAuthorized)
        XCTAssertEqual(verdict(nil), .rejectUnverifiable)
        XCTAssertEqual(verdict(owner, stored: 10, incoming: 10), .rejectStale)
        XCTAssertEqual(
            GroupSettingGate.classify(
                identical: true, incomingUpdatedAtMs: 1, storedUpdatedAtMs: 1, senderKey: member,
                authority: authority, isNewer: { $0 > $1 }),
            .acceptIdentical)
    }

    func testStoredReactionsRoundTrip() {
        XCTAssertEqual(StoredGroupReactions(.only(emoji: ["👍", "🙏"]), updatedAtMs: 3).allowed,
                       .only(emoji: ["👍", "🙏"]))
        XCTAssertEqual(StoredGroupReactions(.off, updatedAtMs: 3).allowed, .off)
        XCTAssertEqual(StoredGroupReactions(.all, updatedAtMs: 3).allowed, .all)
    }

    // MARK: - `:` suggestions (section 13)

    func testAColonQueryNeedsTwoLettersAtTheStartOfAWord() {
        XCTAssertEqual(ColonQuery.find(in: "hi :sm", caretUtf16: nil)?.query, "sm")
        XCTAssertEqual(ColonQuery.find(in: "hi :sm", caretUtf16: nil)?.range, 3..<6)
        XCTAssertNil(ColonQuery.find(in: "hi :s", caretUtf16: nil))
        XCTAssertNil(ColonQuery.find(in: "a:smile", caretUtf16: nil))
        XCTAssertNil(ColonQuery.find(in: "hi :sm ", caretUtf16: nil))
    }

    func testAColonReplacementPutsTheEmojiWhereTheWordWas() {
        var text = "hi :sm"
        let caret = ColonQuery.replace(3..<6, in: &text, with: "😀")
        XCTAssertEqual(text, "hi 😀")
        XCTAssertEqual(caret, 3 + "😀".utf16.count)
    }

    // MARK: - The bounded avatar store (UX 4.5.5)

    func testTheLeastRecentlyDrawnAvatarGoesFirst() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("avatar-cache-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let cache = AvatarDiskCache(directory: dir)
        _ = try cache.write(Data(repeating: 1, count: 100), blobIdHex: "aa", kind: .still)
        _ = try cache.write(Data(repeating: 2, count: 100), blobIdHex: "bb", kind: .still)
        // "aa" drawn after "bb" was written: "bb" is the older one.
        let old = Date(timeIntervalSinceNow: -3600)
        try FileManager.default.setAttributes(
            [.modificationDate: old], ofItemAtPath: cache.url(blobIdHex: "bb", kind: .still).path)
        cache.evictIfNeeded(maxBytes: 150)
        XCTAssertNotNil(cache.existing(blobIdHex: "aa", kind: .still))
        XCTAssertNil(cache.existing(blobIdHex: "bb", kind: .still))
    }

    // MARK: - Knock version (ADR-0087 section 5)

    func testRemovingAnAdminBumpsTheKnockVersionAndAddingOneDoesNot() {
        let a = Data(repeating: 1, count: 32)
        let b = Data(repeating: 2, count: 32)
        let row = KnockVersionStore.Row(version: 0, admins: [a, b], sent: ["x|0"])
        let added = KnockVersionStore.reconciled(row, currentAdmins: [a, b, Data(repeating: 3, count: 32)])
        XCTAssertEqual(added.version, 0)
        XCTAssertEqual(added.sent, ["x|0"])
        let removed = KnockVersionStore.reconciled(row, currentAdmins: [a])
        XCTAssertEqual(removed.version, 1)
        XCTAssertEqual(removed.sent, [])
    }

    func testARotationKeepsOlderVersionsOnThePredecessorsRoot() {
        let row = KnockVersionStore.Row(version: 2, admins: [], sent: [])
        let rotated = KnockVersionStore.rotated(row, predecessorKeyB64: "P1")
        XCTAssertEqual(rotated.version, 3)
        XCTAssertEqual(KnockVersionStore.predecessorKey(for: 0, row: rotated), "P1")
        XCTAssertEqual(KnockVersionStore.predecessorKey(for: 2, row: rotated), "P1")
        XCTAssertNil(KnockVersionStore.predecessorKey(for: 3, row: rotated))
        let twice = KnockVersionStore.rotated(rotated, predecessorKeyB64: "P2")
        XCTAssertEqual(KnockVersionStore.predecessorKey(for: 1, row: twice), "P1")
        XCTAssertEqual(KnockVersionStore.predecessorKey(for: 3, row: twice), "P2")
        XCTAssertNil(KnockVersionStore.predecessorKey(for: 4, row: twice))
    }
}
