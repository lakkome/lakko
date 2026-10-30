// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
@testable import Lakko

/// `docs/ux/second-device.md` section 3, "The list follows the account": an
/// open Devices screen reads the roster again whenever it changes, and only
/// once the persisted snapshot it reads holds the change.
@MainActor
final class DeviceRosterChangesTests: XCTestCase {

    private func heard(
        by account: String, after post: () -> Void, file: StaticString = #filePath, line: UInt = #line
    ) async -> Int {
        var count = 0
        let held = DeviceRosterChanges.publisher(accountIdB64: account).sink { count += 1 }
        post()
        // The publisher delivers on the next turn of the main queue.
        try? await Task.sleep(nanoseconds: 30_000_000)
        held.cancel()
        return count
    }

    func testAScreenHearsItsOwnAccountOnly() async {
        let own = await heard(by: "own") { DeviceRosterChanges.note(accountIdB64: "own") }
        XCTAssertEqual(own, 1)
        let other = await heard(by: "own") { DeviceRosterChanges.note(accountIdB64: "other") }
        XCTAssertEqual(other, 0)
    }

    func testTheReaderRunsAfterThePosterLeft() async {
        var order: [String] = []
        let held = DeviceRosterChanges.publisher(accountIdB64: "own").sink { order.append("read") }
        DeviceRosterChanges.note(accountIdB64: "own")
        order.append("posted")
        try? await Task.sleep(nanoseconds: 30_000_000)
        held.cancel()
        XCTAssertEqual(order, ["posted", "read"])
    }

    /// Every commit of this device, the code route's and the QR route's among
    /// them, goes through the outbox's `announce`.
    func testACommitOfThisDeviceIsAnnounced() async {
        let ports = DeviceGroupCommits.ports(
            accountIdBytes: Data(repeating: 1, count: 16), accountIdB64: "commit-here",
            ds: DsClient(baseURL: Offline.dsBaseURL, session: Offline.session()))
        let entry = DeviceGroupCommitEntry(
            kind: .add, journalIdB64: "", epoch: 2, epochKeyB64: "", rowB64: "", nextSnapshotB64: "",
            createdAtMs: 1)
        let count = await heard(by: "commit-here") { ports.announce(entry) }
        XCTAssertEqual(count, 1)
    }

    func testAReceivedCommitIsSaidAfterTheWriteThatHoldsIt() {
        var change = RosterChangeAfterPersist()
        XCTAssertFalse(change.persisted(), "a batch with no commit says nothing")

        change.commitApplied()
        // A failed persist reaches no `persisted()`: the next batch's does.
        change.commitApplied()
        XCTAssertTrue(change.persisted())
        XCTAssertFalse(change.persisted(), "once")
    }

    /// The screen is SwiftUI and has no test host here: what it reads is
    /// pinned at the source. A read that fails must keep the rows.
    func testTheOpenListFollowsTheRoster() throws {
        let screen = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Lakko/UI/Devices/DevicesScreen.swift")
        guard let source = try? String(contentsOf: screen, encoding: .utf8) else {
            throw XCTSkip("the app's sources are not readable from this test run")
        }
        XCTAssertTrue(source.contains(".onReceive(DeviceRosterChanges.publisher(accountIdB64: accountIdB64))"))
        let follow = try XCTUnwrap(source.range(of: "private func followRoster()"))
        XCTAssertFalse(source[follow.upperBound...].prefix(500).contains("state = .error"))
    }
}
