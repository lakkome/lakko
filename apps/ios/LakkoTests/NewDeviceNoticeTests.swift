// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// UX `docs/ux/identity-recovery.md` sections 17.5 / 17.6 — "A device was
/// added to your account": the pending rule with the synced acknowledged
/// set, its storage, and the center the badges, the Devices screen and the
/// system notification draw from.
@MainActor
final class NewDeviceNoticeTests: XCTestCase {

    typealias Rule = NewDeviceNoticeRule

    private func observe(
        _ record: NewDeviceNoticeRecord?, _ roster: [String], acked: Set<String> = [], at nowMs: Int64
    ) -> NewDeviceNoticeRecord {
        Rule.observe(record: record, roster: roster, ownKey: "me", acknowledged: acked, nowMs: nowMs)
    }

    // MARK: - The rule

    func testTheFirstReadingKnowsEveryDevicePresent() {
        let record = observe(nil, ["me", "a", "b"], at: 10)
        XCTAssertEqual(record.known, ["me", "a", "b"])
        XCTAssertTrue(record.pending.isEmpty, "a device never warns about what it joined to")
        XCTAssertFalse(Rule.badgeVisible(record: record, acknowledged: []))
    }

    func testADeviceThatJoinsLaterIsPendingAndRaisesTheBadge() {
        let seeded = observe(nil, ["me", "a"], at: 10)
        let grown = observe(seeded, ["me", "a", "new"], at: 50)
        XCTAssertEqual(grown.pending, [.init(key: "new", learnedAtMs: 50)])
        XCTAssertTrue(Rule.badgeVisible(record: grown, acknowledged: []))
        XCTAssertEqual(Rule.explanation(grown.pending), .init(count: 1, latestMs: 50))
        XCTAssertEqual(Rule.explanation(grown.pending)?.titleKey, "devices.added_notice.title")
    }

    func testReadingTheSameRosterAgainKeepsTheFirstTime() {
        let seeded = observe(nil, ["me"], at: 10)
        let grown = observe(seeded, ["me", "new"], at: 50)
        XCTAssertEqual(observe(grown, ["me", "new"], at: 90), grown)
    }

    func testTwoNewDevicesUseThePluralTitleAndTheLatestTime() {
        var record = observe(nil, ["me"], at: 10)
        record = observe(record, ["me", "x"], at: 20)
        record = observe(record, ["me", "x", "y"], at: 70)
        XCTAssertEqual(Rule.explanation(record.pending), .init(count: 2, latestMs: 70))
        XCTAssertEqual(Rule.explanation(record.pending)?.titleKey, "devices.added_notice.title_many")
    }

    func testAnAcknowledgedDeviceIsNeverPending() {
        let seeded = observe(nil, ["me"], at: 10)
        let grown = observe(seeded, ["me", "new"], at: 50)
        XCTAssertTrue(Rule.pending(record: grown, acknowledged: ["new"]).isEmpty)
        XCTAssertFalse(Rule.badgeVisible(record: grown, acknowledged: ["new"]))
    }

    func testADeviceLearnedAfterItWasAcknowledgedElsewhereShowsNothing() {
        let seeded = observe(nil, ["me"], at: 10)
        let grown = observe(seeded, ["me", "new"], acked: ["new"], at: 50)
        XCTAssertTrue(grown.pending.isEmpty)
        XCTAssertTrue(grown.known.contains("new"))
        XCTAssertEqual(
            Rule.notification(before: [], after: Rule.pending(record: grown, acknowledged: ["new"]).map(\.key)),
            .none)
    }

    func testReceivingAnAcknowledgementMovesThePendingDeviceToKnown() {
        let seeded = observe(nil, ["me"], at: 10)
        let grown = observe(seeded, ["me", "new"], at: 50)
        let absorbed = Rule.absorb(record: grown, acknowledged: ["new"])
        XCTAssertTrue(absorbed.pending.isEmpty)
        XCTAssertTrue(absorbed.known.contains("new"))
        XCTAssertEqual(Rule.notification(before: ["new"], after: []), .withdraw)
    }

    func testTheDeviceThatLetItInShowsNothing() {
        var record = observe(nil, ["me"], at: 10)
        record = Rule.letIn(record: record, key: "new")
        record = observe(record, ["me", "new"], at: 50)
        XCTAssertTrue(record.pending.isEmpty)
        XCTAssertTrue(record.known.contains("new"))
        XCTAssertTrue(record.letIn.isEmpty)
    }

    func testLettingInAfterTheRosterShowedItEndsTheWarning() {
        var record = observe(nil, ["me"], at: 10)
        record = observe(record, ["me", "new"], at: 50)
        record = Rule.letIn(record: record, key: "new")
        XCTAssertFalse(Rule.badgeVisible(record: record, acknowledged: []))
    }

    func testTheOwnKeyIsNeverNew() {
        let seeded = Rule.observe(record: nil, roster: ["a"], ownKey: nil, acknowledged: [], nowMs: 10)
        let grown = observe(seeded, ["a", "me"], at: 50)
        XCTAssertTrue(grown.pending.isEmpty)
    }

    func testOpeningDevicesWithPendingAcknowledgesEveryPendingDevice() {
        var record = observe(nil, ["me"], at: 10)
        record = observe(record, ["me", "x"], at: 20)
        record = observe(record, ["me", "x", "y"], at: 30)
        let visit = Rule.openDevices(record: record, acknowledged: [])
        XCTAssertEqual(visit?.keysToAcknowledge, ["x", "y"])
        XCTAssertEqual(visit?.highlighted.map(\.key), ["x", "y"])
        XCTAssertNil(Rule.openDevices(record: record, acknowledged: ["x", "y"]), "nothing pending: no visit")
        XCTAssertEqual(Rule.openDevices(record: record, acknowledged: ["x"])?.keysToAcknowledge, ["y"])
    }

    func testTheNotificationFollowsThePendingSet() {
        XCTAssertEqual(Rule.notification(before: [], after: ["a"]), .post(count: 1))
        XCTAssertEqual(Rule.notification(before: ["a"], after: ["a", "b"]), .post(count: 2))
        XCTAssertEqual(Rule.notification(before: ["a", "b"], after: ["a"]), .none)
        XCTAssertEqual(Rule.notification(before: ["a"], after: []), .withdraw)
        XCTAssertEqual(Rule.notification(before: [], after: []), .none)
        XCTAssertEqual(Rule.notification(before: ["a"], after: ["a"]), .none)
    }

    func testRemovingANewDeviceRaisesTheChangePasswordLine() {
        var record = observe(nil, ["me", "old"], at: 10)
        record = observe(record, ["me", "old", "new"], at: 50)
        record = Rule.removed(record: record, key: "new", wasHighlighted: false)
        XCTAssertTrue(record.pending.isEmpty)
        XCTAssertTrue(record.removedNewDevice)
        record = Rule.dismissRemovedLine(record: record)
        XCTAssertFalse(record.removedNewDevice)
    }

    func testRemovingARowHighlightedByTheVisitRaisesTheLineToo() {
        let record = observe(nil, ["me", "new"], at: 10)
        XCTAssertTrue(Rule.removed(record: record, key: "new", wasHighlighted: true).removedNewDevice)
        XCTAssertFalse(Rule.removed(record: record, key: "new", wasHighlighted: false).removedNewDevice)
    }

    func testADeviceRemovedElsewhereEndsItsWarningQuietly() {
        var record = observe(nil, ["me"], at: 10)
        record = observe(record, ["me", "new"], at: 50)
        record = observe(record, ["me"], at: 90)
        XCTAssertTrue(record.pending.isEmpty)
        XCTAssertFalse(record.removedNewDevice, "this device did not remove it")
    }

    func testARemovedThenReAddedDeviceIsNewAgain() {
        var record = observe(nil, ["me", "old"], at: 10)
        record = Rule.removed(record: record, key: "old", wasHighlighted: false)
        record = observe(record, ["me"], at: 20)
        record = observe(record, ["me", "old"], at: 30)
        XCTAssertEqual(record.pending.map(\.key), ["old"])
    }

    // MARK: - Storage and the center

    private final class RecordingNotifier: NewDeviceNotifier {
        var posts: [Int] = []
        var withdrawals = 0
        func post(accountIdB64: String, count: Int) { posts.append(count) }
        func withdraw(accountIdB64: String) { withdrawals += 1 }
    }

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

    private func center(_ notifier: RecordingNotifier = RecordingNotifier()) -> NewDeviceNoticeCenter {
        NewDeviceNoticeCenter(store: store(), nowMs: { 1_000 }, notifier: notifier)
    }

    func testANewDeviceIsNotifiedAndBadgedAndSurvivesARestart() {
        let notifier = RecordingNotifier()
        let first = center(notifier)
        first.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        XCTAssertEqual(notifier.posts, [], "the seeding reading raises nothing")
        first.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertEqual(notifier.posts, [1], "posted whether the app is in front or not")
        XCTAssertTrue(first.badgeVisible)
        XCTAssertTrue(first.isHighlighted(signaturePublicKey: key(2)))
        XCTAssertFalse(first.isHighlighted(signaturePublicKey: key(1)))

        // A new process: still pending tomorrow, not notified again.
        let again = RecordingNotifier()
        let second = center(again)
        second.bind(accountIdB64: "acct")
        XCTAssertTrue(second.badgeVisible)
        second.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertEqual(again.posts, [])
        second.bind(accountIdB64: "other")
        XCTAssertFalse(second.badgeVisible, "another account's record is its own")
    }

    func testAnAcknowledgementFromAnotherDeviceClearsEverything() {
        let notifier = RecordingNotifier()
        let c = center(notifier)
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.setAcknowledged(accountIdB64: "acct", keys: [key(2)])
        XCTAssertFalse(c.badgeVisible)
        XCTAssertFalse(c.isHighlighted(signaturePublicKey: key(2)))
        XCTAssertEqual(notifier.withdrawals, 1)
        XCTAssertTrue(c.record?.known.contains(key(2).base64EncodedString()) ?? false)
    }

    func testAPendingDeviceAcknowledgedWhileTheAppWasClosedIsWithdrawnAtTheNextOpen() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        let notifier = RecordingNotifier()
        let reopened = center(notifier)
        reopened.setAcknowledged(accountIdB64: "acct", keys: [key(2)])
        XCTAssertEqual(notifier.withdrawals, 1)
        XCTAssertFalse(reopened.badgeVisible)
    }

    func testADeviceAcknowledgedBeforeThisDeviceLearnedOfItShowsNothing() {
        let notifier = RecordingNotifier()
        let c = center(notifier)
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.setAcknowledged(accountIdB64: "acct", keys: [key(2)])
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertEqual(notifier.posts, [])
        XCTAssertFalse(c.badgeVisible)
    }

    func testOpeningDevicesAcknowledgesAndKeepsTheHighlightForTheVisit() {
        let notifier = RecordingNotifier()
        let c = center(notifier)
        var sent: [[Data]] = []
        c.setAcknowledger(accountIdB64: "acct") { sent.append($0) }
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2), key(3)], ownKey: key(1))

        c.beginVisit()
        XCTAssertEqual(sent, [[key(2), key(3)]], "every pending device, for the account")
        XCTAssertFalse(c.badgeVisible, "the badge goes at once")
        XCTAssertEqual(notifier.withdrawals, 1, "and so does the notification")
        XCTAssertTrue(c.isHighlighted(signaturePublicKey: key(2)), "the rows stay highlighted for this visit")
        XCTAssertEqual(c.explanation, .init(count: 2, latestMs: 1_000))

        // The synced fold catching up changes nothing on screen.
        c.setAcknowledged(accountIdB64: "acct", keys: [key(2), key(3)])
        XCTAssertTrue(c.isHighlighted(signaturePublicKey: key(3)))

        c.endVisit()
        XCTAssertFalse(c.isHighlighted(signaturePublicKey: key(2)))
        XCTAssertNil(c.explanation)
        c.beginVisit()
        XCTAssertEqual(sent.count, 1, "nothing pending: a later visit acknowledges nothing")
        XCTAssertNil(c.explanation)
    }

    func testADeviceThatJoinsWhileDevicesIsOpenIsSeenThere() {
        let notifier = RecordingNotifier()
        let c = center(notifier)
        var sent: [[Data]] = []
        c.setAcknowledger(accountIdB64: "acct") { sent.append($0) }
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.beginVisit()
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertEqual(sent, [[key(2)]])
        XCTAssertEqual(notifier.posts, [], "the person is looking at the list")
        XCTAssertTrue(c.isHighlighted(signaturePublicKey: key(2)))
        XCTAssertFalse(c.badgeVisible)
    }

    func testOpeningDevicesBeforeTheSyncedStateIsReadyConsumesNothing() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.beginVisit()
        XCTAssertTrue(c.badgeVisible, "nothing consumed while the synced state is not ready")
        XCTAssertEqual(c.pending.map(\.key), [key(2).base64EncodedString()])
        XCTAssertTrue(c.isHighlighted(signaturePublicKey: key(2)))
        // Ready while the screen is still open: acknowledged then.
        var sent: [[Data]] = []
        c.setAcknowledger(accountIdB64: "acct") { sent.append($0) }
        XCTAssertEqual(sent, [[key(2)]])
        XCTAssertFalse(c.badgeVisible)
        XCTAssertTrue(c.isHighlighted(signaturePublicKey: key(2)), "still highlighted for this visit")
    }

    func testReadyAfterTheScreenWasLeftAcknowledgesNothing() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.beginVisit()
        c.endVisit()
        var sent: [[Data]] = []
        c.setAcknowledger(accountIdB64: "acct") { sent.append($0) }
        XCTAssertEqual(sent, [])
        XCTAssertTrue(c.badgeVisible)
    }

    func testLettingADeviceInSuppressesItsWarningAndItsNotification() {
        let notifier = RecordingNotifier()
        let c = center(notifier)
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.letIn(accountIdB64: "acct", signaturePublicKey: key(2))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        XCTAssertFalse(c.badgeVisible)
        XCTAssertEqual(notifier.posts, [])
    }

    func testRemovingANewDeviceShowsTheChangePasswordLineUntilDismissed() {
        let c = center()
        c.observe(accountIdB64: "acct", roster: [key(1)], ownKey: key(1))
        c.observe(accountIdB64: "acct", roster: [key(1), key(2)], ownKey: key(1))
        c.beginVisit()
        c.removed(signaturePublicKey: key(2))
        XCTAssertNil(c.explanation)
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

    func testTheNotificationIdentifierIsFixedPerAccount() {
        XCTAssertEqual(
            NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: "a"),
            NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: "a"))
        XCTAssertNotEqual(
            NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: "a"),
            NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: "b"))
    }

    func testEveryKeyTheWarningNamesIsInTheCatalog() {
        for key in [
            "devices.added_notice.title", "devices.added_notice.title_many",
            "devices.added_notice.not_you", "devices.added_notice.notification_body",
            "common.dismiss", "settings.devices.new_badge",
            "settings.devices.removed_change_password", "settings.password.change",
            "nav.settings.attention_a11y", "settings.devices.attention_a11y",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        XCTAssertNotEqual(
            Localization.text("devices.added_notice.when", texts: ["date": "6 Oct 2026, 14:05"]),
            "devices.added_notice.when")
    }
}
