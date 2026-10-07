// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// UX `docs/ux/identity-recovery.md` section 17.5 — "A device was added to
/// your account": the acknowledge / suppress rule, its storage and the
/// center the banners draw from.
@MainActor
final class NewDeviceNoticeTests: XCTestCase {

    typealias Rule = NewDeviceNoticeRule

    // MARK: - The rule

    func testTheFirstReadingKnowsEveryDevicePresent() {
        let record = Rule.observe(record: nil, roster: ["me", "a", "b"], ownKey: "me", nowMs: 10)
        XCTAssertEqual(record.acknowledged, ["me", "a", "b"])
        XCTAssertTrue(record.pending.isEmpty, "a device never shows the notice about what it joined to")
        XCTAssertNil(Rule.banner(record: record))
    }

    func testADeviceThatJoinsLaterRaisesTheNotice() {
        let seeded = Rule.observe(record: nil, roster: ["me", "a"], ownKey: "me", nowMs: 10)
        let grown = Rule.observe(record: seeded, roster: ["me", "a", "new"], ownKey: "me", nowMs: 50)
        XCTAssertEqual(grown.pending, [.init(key: "new", learnedAtMs: 50)])
        XCTAssertEqual(Rule.banner(record: grown), .init(count: 1, latestMs: 50))
        XCTAssertEqual(Rule.banner(record: grown)?.titleKey, "devices.added_notice.title")
        XCTAssertTrue(Rule.isNew(record: grown, key: "new"))
        XCTAssertFalse(Rule.isNew(record: grown, key: "a"))
    }

    func testReadingTheSameRosterAgainKeepsTheFirstTime() {
        let seeded = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        let grown = Rule.observe(record: seeded, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        let again = Rule.observe(record: grown, roster: ["me", "new"], ownKey: "me", nowMs: 90)
        XCTAssertEqual(again, grown)
    }

    func testTwoNewDevicesUseThePluralTitleAndTheLatestTime() {
        var record = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        record = Rule.observe(record: record, roster: ["me", "x"], ownKey: "me", nowMs: 20)
        record = Rule.observe(record: record, roster: ["me", "x", "y"], ownKey: "me", nowMs: 70)
        XCTAssertEqual(Rule.banner(record: record), .init(count: 2, latestMs: 70))
        XCTAssertEqual(Rule.banner(record: record)?.titleKey, "devices.added_notice.title_many")
    }

    func testTheDeviceThatLetItInShowsNoNotice() {
        var record = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        record = Rule.letIn(record: record, key: "new")
        record = Rule.observe(record: record, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        XCTAssertTrue(record.pending.isEmpty)
        XCTAssertTrue(record.acknowledged.contains("new"))
        XCTAssertTrue(record.letIn.isEmpty)
    }

    func testLettingInAfterTheRosterShowedItWithdrawsTheNotice() {
        var record = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        record = Rule.observe(record: record, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        XCTAssertNotNil(Rule.banner(record: record))
        record = Rule.letIn(record: record, key: "new")
        XCTAssertNil(Rule.banner(record: record))
        XCTAssertTrue(record.acknowledged.contains("new"))
    }

    func testTheOwnKeyIsNeverNew() {
        let seeded = Rule.observe(record: nil, roster: ["a"], ownKey: nil, nowMs: 10)
        let grown = Rule.observe(record: seeded, roster: ["a", "me"], ownKey: "me", nowMs: 50)
        XCTAssertTrue(grown.pending.isEmpty)
    }

    func testDismissAcknowledgesAndTheBadgeGoesWithTheBanner() {
        var record = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        record = Rule.observe(record: record, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        record = Rule.dismiss(record: record)
        XCTAssertNil(Rule.banner(record: record))
        XCTAssertFalse(Rule.isNew(record: record, key: "new"))
        let again = Rule.observe(record: record, roster: ["me", "new"], ownKey: "me", nowMs: 90)
        XCTAssertNil(Rule.banner(record: again), "not raised again")
    }

    func testRemovingANewDeviceRaisesTheChangePasswordLine() {
        var record = Rule.observe(record: nil, roster: ["me", "old"], ownKey: "me", nowMs: 10)
        record = Rule.observe(record: record, roster: ["me", "old", "new"], ownKey: "me", nowMs: 50)
        record = Rule.removed(record: record, key: "new")
        XCTAssertNil(Rule.banner(record: record))
        XCTAssertTrue(record.removedNewDevice)
        record = Rule.dismissRemovedLine(record: record)
        XCTAssertFalse(record.removedNewDevice)
    }

    func testRemovingAKnownDeviceRaisesNothing() {
        var record = Rule.observe(record: nil, roster: ["me", "old"], ownKey: "me", nowMs: 10)
        record = Rule.removed(record: record, key: "old")
        XCTAssertFalse(record.removedNewDevice)
        XCTAssertFalse(record.acknowledged.contains("old"))
    }

    func testADeviceRemovedElsewhereEndsItsNoticeQuietly() {
        var record = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        record = Rule.observe(record: record, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        record = Rule.observe(record: record, roster: ["me"], ownKey: "me", nowMs: 90)
        XCTAssertNil(Rule.banner(record: record))
        XCTAssertFalse(record.removedNewDevice, "this device did not remove it")
    }

    func testARemovedThenReAddedDeviceIsNewAgain() {
        var record = Rule.observe(record: nil, roster: ["me", "old"], ownKey: "me", nowMs: 10)
        record = Rule.removed(record: record, key: "old")
        record = Rule.observe(record: record, roster: ["me"], ownKey: "me", nowMs: 20)
        record = Rule.observe(record: record, roster: ["me", "old"], ownKey: "me", nowMs: 30)
        XCTAssertTrue(Rule.isNew(record: record, key: "old"))
    }

    func testGainedPendingIsTheNotificationsTrigger() {
        let seeded = Rule.observe(record: nil, roster: ["me"], ownKey: "me", nowMs: 10)
        let grown = Rule.observe(record: seeded, roster: ["me", "new"], ownKey: "me", nowMs: 50)
        XCTAssertFalse(Rule.gainedPending(before: nil, after: seeded))
        XCTAssertTrue(Rule.gainedPending(before: seeded, after: grown))
        XCTAssertFalse(Rule.gainedPending(before: grown, after: grown))
        XCTAssertFalse(Rule.gainedPending(before: grown, after: Rule.dismiss(record: grown)))
    }

    // MARK: - Storage and the center

    private var suite: String!

    override func setUp() {
        super.setUp()
        suite = "new-device-notice-\(UUID().uuidString)"
    }

    override func tearDown() {
        UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    private func store() -> NewDeviceNoticeStore {
        NewDeviceNoticeStore(defaults: UserDefaults(suiteName: suite)!)
    }

    private func key(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    private func center(notified: @escaping () -> Void = {}) -> NewDeviceNoticeCenter {
        NewDeviceNoticeCenter(store: store(), nowMs: { 1_000 }, notifyIfNotInFront: notified)
    }

    func testTheRecordSurvivesARestartAndIsPerAccount() {
        var notified = 0
        let first = center { notified += 1 }
        first.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        XCTAssertEqual(notified, 0, "the seeding reading raises nothing")
        first.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertEqual(notified, 1)
        XCTAssertEqual(first.banner, .init(count: 1, latestMs: 1_000))
        XCTAssertTrue(first.isNew(signaturePublicKey: key(2)))

        // A new process: the banner is still there tomorrow.
        let second = center()
        second.bind(accountIdB64: "acct")
        XCTAssertEqual(second.banner, .init(count: 1, latestMs: 1_000))
        second.bind(accountIdB64: "other")
        XCTAssertNil(second.banner, "another account's record is its own")
        XCTAssertNil(store().load(accountIdB64: "other"))
    }

    func testDismissIsKeptAndNotNotifiedAgain() {
        var notified = 0
        let c = center { notified += 1 }
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.dismiss()
        XCTAssertNil(c.banner)
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertNil(c.banner)
        XCTAssertEqual(notified, 1)
        XCTAssertNil(center().banner, "unbound: nothing shown")
    }

    func testLettingADeviceInSuppressesItsNoticeAndItsNotification() {
        var notified = 0
        let c = center { notified += 1 }
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.letIn(accountIdB64: "acct", signaturePublicKey: key(2))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertNil(c.banner)
        XCTAssertEqual(notified, 0)
    }

    func testRemovingANewDeviceShowsTheChangePasswordLineUntilDismissed() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.removed(signaturePublicKey: key(2))
        XCTAssertNil(c.banner)
        XCTAssertTrue(c.showsRemovedLine)
        c.dismissRemovedLine()
        XCTAssertFalse(c.showsRemovedLine)
    }

    func testTheWipeRemovesTheRecord() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        XCTAssertNotNil(store().load(accountIdB64: "acct"))
        store().clear(accountIdB64: "acct")
        XCTAssertNil(store().load(accountIdB64: "acct"))
    }

    func testEveryKeyTheNoticeNamesIsInTheCatalog() {
        for key in [
            "devices.added_notice.title", "devices.added_notice.title_many",
            "devices.added_notice.not_you", "devices.added_notice.open_devices",
            "devices.added_notice.notification_body", "common.dismiss", "settings.devices.new_badge",
            "settings.devices.removed_change_password", "settings.password.change",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        XCTAssertNotEqual(
            Localization.text("devices.added_notice.when", texts: ["date": "6 Oct 2026, 14:05"]),
            "devices.added_notice.when")
    }
}
