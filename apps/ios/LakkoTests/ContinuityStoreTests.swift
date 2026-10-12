// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0065 R.5 / R.7 - this device's own facts about a moved conversation
/// (`ContinuityStore`, the twin of `apps/web/src/chat/continuity-store.ts`,
/// which has no test file of its own).
final class ContinuityStoreTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults = .standard

    override func setUp() {
        super.setUp()
        suiteName = "ContinuityStoreTests." + UUID().uuidString
        defaults = UserDefaults(suiteName: suiteName) ?? .standard
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func store(_ account: String = "acct") -> ContinuityStore {
        ContinuityStore(defaults: defaults, ownerAccountIdB64: account)
    }

    func testTheFirstTimeAConversationMovedWins() {
        XCTAssertNil(store().conversationMovedAt(oldGroupIdB64: "G"))
        store().noteConversationMoved(oldGroupIdB64: "G", nowMs: 1_000)
        store().noteConversationMoved(oldGroupIdB64: "G", nowMs: 2_000)
        XCTAssertEqual(store().conversationMovedAt(oldGroupIdB64: "G"), 1_000)
        XCTAssertNil(store().conversationMovedAt(oldGroupIdB64: "H"))
    }

    func testTheResendAndPushRetiredMarksArePerGroup() {
        XCTAssertFalse(store().wasResent(oldGroupIdB64: "G"))
        XCTAssertFalse(store().wasPushRetired(oldGroupIdB64: "G"))
        store().markResent(oldGroupIdB64: "G")
        store().markPushRetired(oldGroupIdB64: "H")
        XCTAssertTrue(store().wasResent(oldGroupIdB64: "G"))
        XCTAssertFalse(store().wasResent(oldGroupIdB64: "H"))
        XCTAssertTrue(store().wasPushRetired(oldGroupIdB64: "H"))
        XCTAssertFalse(store().wasPushRetired(oldGroupIdB64: "G"))
    }

    func testAwaitedSwapsAreReplacedAndAnEmptySetRemovesTheEntry() {
        XCTAssertEqual(store().awaitedSwaps(groupIdB64: "G"), [:])
        store().setAwaitedSwaps(groupIdB64: "G", ["anna": 10, "bob": 20])
        XCTAssertEqual(store().awaitedSwaps(groupIdB64: "G"), ["anna": 10, "bob": 20])
        store().setAwaitedSwaps(groupIdB64: "G", ["bob": 20])
        XCTAssertEqual(store().awaitedSwaps(groupIdB64: "G"), ["bob": 20])
        store().setAwaitedSwaps(groupIdB64: "G", [:])
        XCTAssertEqual(store().awaitedSwaps(groupIdB64: "G"), [:])
    }

    func testTheFactsArePerAccountAndClearForgetsOneAccount() {
        store("a").markResent(oldGroupIdB64: "G")
        store("b").noteConversationMoved(oldGroupIdB64: "G", nowMs: 5)
        XCTAssertFalse(store("b").wasResent(oldGroupIdB64: "G"))
        XCTAssertNil(store("a").conversationMovedAt(oldGroupIdB64: "G"))
        ContinuityStore.clear(ownerAccountIdB64: "a", defaults: defaults)
        XCTAssertFalse(store("a").wasResent(oldGroupIdB64: "G"))
        XCTAssertEqual(store("b").conversationMovedAt(oldGroupIdB64: "G"), 5)
    }

    func testOneFactDoesNotDisturbAnother() {
        store().noteConversationMoved(oldGroupIdB64: "G", nowMs: 7)
        store().markResent(oldGroupIdB64: "G")
        store().setAwaitedSwaps(groupIdB64: "G", ["anna": 3])
        store().markPushRetired(oldGroupIdB64: "G")
        XCTAssertEqual(store().conversationMovedAt(oldGroupIdB64: "G"), 7)
        XCTAssertTrue(store().wasResent(oldGroupIdB64: "G"))
        XCTAssertEqual(store().awaitedSwaps(groupIdB64: "G"), ["anna": 3])
        XCTAssertTrue(store().wasPushRetired(oldGroupIdB64: "G"))
    }

    func testReadsTheRowTheWebTwinWritesAndSkipsJunk() {
        let json = #"{"moved":{"G":1000,"X":"soon"},"resent":{"G":true,"X":1},"swapping":{"G":{"anna":10,"bob":null},"X":7},"pushRetired":"no"}"#
        let row = ContinuityStore.decode(Data(json.utf8))
        XCTAssertEqual(row.moved, ["G": 1000])
        XCTAssertEqual(row.resent, ["G"])
        XCTAssertEqual(row.swapping, ["G": ["anna": 10]])
        XCTAssertTrue(row.pushRetired.isEmpty)
        XCTAssertEqual(ContinuityStore.decode(Data("not json".utf8)), ContinuityStore.Row())
        XCTAssertEqual(ContinuityStore.decode(Data("null".utf8)), ContinuityStore.Row())
    }

    func testTheRowRoundTrips() throws {
        let row = ContinuityStore.Row(
            moved: ["G": 1], resent: ["G", "H"], swapping: ["G": ["anna": 2]], pushRetired: ["H"])
        let data = try XCTUnwrap(ContinuityStore.encode(row))
        XCTAssertEqual(ContinuityStore.decode(data), row)
    }
}
