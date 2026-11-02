// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/group-and-channel-privacy.md` section 10 — a conversation with no
/// message yet sits where it arrived.
final class GroupArrivalTests: XCTestCase {

    private func suite() throws -> UserDefaults {
        try XCTUnwrap(UserDefaults(suiteName: "group-arrival-\(UUID().uuidString)"))
    }

    func testTheLastMessageDecidesAndTheArrivalOnlyFillsIn() {
        XCTAssertEqual(GroupArrival.activityMs(lastMessageMs: 100, arrivedAtMs: 900), 100,
                       "a chat joined with its history keeps its place in the list")
        XCTAssertEqual(GroupArrival.activityMs(lastMessageMs: nil, arrivedAtMs: 900), 900)
        XCTAssertEqual(GroupArrival.activityMs(lastMessageMs: 100, arrivedAtMs: nil), 100)
        XCTAssertNil(GroupArrival.activityMs(lastMessageMs: nil, arrivedAtMs: nil))
    }

    func testANewGroupSitsAboveAnOlderChatInTheList() {
        let saturday = ConversationListRow(
            id: "saturday", kind: .chat, title: "Anna", preview: "hello",
            lastActivityMs: GroupArrival.activityMs(lastMessageMs: 1_000, arrivedAtMs: 500))
        let fresh = ConversationListRow(
            id: "fresh", kind: .group, title: "Hikers", preview: "",
            lastActivityMs: GroupArrival.activityMs(lastMessageMs: nil, arrivedAtMs: 2_000))
        XCTAssertEqual(ConversationList.sorted([saturday, fresh]).map(\.id), ["fresh", "saturday"])
    }

    func testTheStoreIsPerAccountAndSurvivesARestart() throws {
        let defaults = try suite()
        GroupArrivalStore(defaults: defaults).remember(accountIdB64: "a", groupIdHex: "ab", atMs: 7)
        let next = GroupArrivalStore(defaults: defaults)
        XCTAssertEqual(next.load(accountIdB64: "a"), ["ab": 7])
        XCTAssertTrue(next.load(accountIdB64: "other").isEmpty)
    }

    func testARejoinMovesTheTimeAndARemovalDropsIt() throws {
        let store = GroupArrivalStore(defaults: try suite())
        store.remember(accountIdB64: "a", groupIdHex: "ab", atMs: 7)
        store.remember(accountIdB64: "a", groupIdHex: "ab", atMs: 9)
        store.remember(accountIdB64: "a", groupIdHex: "cd", atMs: 8)
        XCTAssertEqual(store.load(accountIdB64: "a"), ["ab": 9, "cd": 8])
        store.remove(accountIdB64: "a", groupIdHex: "ab")
        XCTAssertEqual(store.load(accountIdB64: "a"), ["cd": 8])
        store.clearForAccount(accountIdB64: "a")
        XCTAssertTrue(store.load(accountIdB64: "a").isEmpty)
    }

    func testASealedAccountTakesNoLateWrite() throws {
        let store = GroupArrivalStore(defaults: try suite())
        let account = "sealed-\(UUID().uuidString)"
        AccountStoreGate.seal(account, wiping: true) {}
        store.remember(accountIdB64: account, groupIdHex: "ab", atMs: 7)
        XCTAssertTrue(store.load(accountIdB64: account).isEmpty)
    }
}
