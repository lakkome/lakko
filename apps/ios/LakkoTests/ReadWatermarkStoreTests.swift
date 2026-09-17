// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0024 on iOS: the durable per-account, per-group read-watermark store
/// behind the Read tick.
final class ReadWatermarkStoreTests: XCTestCase {

    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suite = "ReadWatermarkStoreTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    func testMergeIsMonotone() {
        let store = ReadWatermarkStore(defaults: defaults)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g"), [:])

        XCTAssertEqual(store.merge(accountIdB64: "a", groupIdHex: "g", sender: "bob", upTo: 10), ["bob": 10])
        XCTAssertNil(store.merge(accountIdB64: "a", groupIdHex: "g", sender: "bob", upTo: 7), "a lower value never lowers")
        XCTAssertNil(store.merge(accountIdB64: "a", groupIdHex: "g", sender: "bob", upTo: 10), "a replay is a no-op")
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g"), ["bob": 10])

        XCTAssertEqual(
            store.merge(accountIdB64: "a", groupIdHex: "g", sender: "carol", upTo: 3),
            ["bob": 10, "carol": 3])
        XCTAssertEqual(
            store.merge(accountIdB64: "a", groupIdHex: "g", sender: "bob", upTo: 12),
            ["bob": 12, "carol": 3])
    }

    func testAccountsAndGroupsAreIsolated() {
        let store = ReadWatermarkStore(defaults: defaults)
        store.merge(accountIdB64: "a", groupIdHex: "g1", sender: "bob", upTo: 5)
        store.merge(accountIdB64: "a", groupIdHex: "g2", sender: "bob", upTo: 9)
        store.merge(accountIdB64: "b", groupIdHex: "g1", sender: "bob", upTo: 1)

        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), ["bob": 5])
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g2"), ["bob": 9])
        XCTAssertEqual(store.load(accountIdB64: "b", groupIdHex: "g1"), ["bob": 1])

        store.clear(accountIdB64: "a")
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), [:])
        XCTAssertEqual(store.load(accountIdB64: "b", groupIdHex: "g1"), ["bob": 1], "clear is per account")
    }

    func testPersistsAcrossInstancesAndRemovesOneGroup() {
        let store = ReadWatermarkStore(defaults: defaults)
        store.merge(accountIdB64: "a", groupIdHex: "g1", sender: "bob", upTo: Int64.max)
        store.merge(accountIdB64: "a", groupIdHex: "g2", sender: "carol", upTo: 4)

        let reloaded = ReadWatermarkStore(defaults: UserDefaults(suiteName: suite)!)
        XCTAssertEqual(reloaded.load(accountIdB64: "a", groupIdHex: "g1"), ["bob": Int64.max])
        XCTAssertNil(
            reloaded.merge(accountIdB64: "a", groupIdHex: "g1", sender: "bob", upTo: 0),
            "the monotone floor survives a reload")

        reloaded.remove(accountIdB64: "a", groupIdHex: "g1")
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), [:])
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g2"), ["carol": 4])
    }
}
