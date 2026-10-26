// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The Invite links create form: presets, custom validation and the mapping
/// to `generateInviteLink`'s arguments (the web `invite-custom.test.ts`
/// cases), and the shared expiry rounding of a link row.
final class InviteCreateFormTests: XCTestCase {
    private let day: Int64 = 86_400_000
    private let hour: Int64 = 3_600_000

    func testTheFormOpensOnSevenDaysSingleUse() {
        XCTAssertEqual(
            InviteCreateForm().params,
            .init(label: "", expiry: .fromNow(ms: 7 * day), maxUses: 0))
    }

    func testPresetsMapToTheWireValues() {
        var form = InviteCreateForm()
        form.expiry = .oneDay
        XCTAssertEqual(form.params?.expiry, .fromNow(ms: day))
        form.expiry = .thirtyDays
        XCTAssertEqual(form.params?.expiry, .fromNow(ms: 30 * day))
        form.expiry = .never
        XCTAssertEqual(form.params?.expiry, .never)
        form.uses = .upToTen
        XCTAssertEqual(form.params?.maxUses, 10)
        form.uses = .unlimited
        XCTAssertEqual(form.params?.maxUses, UInt32.max)
        form.uses = .one
        XCTAssertEqual(form.params?.maxUses, ContactsRepository.singleUse)
    }

    func testNeverPutsTheNeverExpiresSentinelOnTheWire() {
        XCTAssertEqual(InviteExpiry.never.expiryMs(nowMs: 1_800_000_000_000), 0)
        XCTAssertEqual(
            InviteExpiry.fromNow(ms: day).expiryMs(nowMs: 1_800_000_000_000),
            1_800_000_000_000 + day)
    }

    func testCustomExpiryAcceptsOneHourToAYear() {
        XCTAssertEqual(InviteCreateForm.customExpiryMs(amount: "1", unit: .hours), hour)
        XCTAssertEqual(InviteCreateForm.customExpiryMs(amount: "12", unit: .days), 12 * day)
        XCTAssertEqual(InviteCreateForm.customExpiryMs(amount: "365", unit: .days), 365 * day)
        XCTAssertEqual(InviteCreateForm.customExpiryMs(amount: " 8760 ", unit: .hours), 8760 * hour)
        for bad in ["", "0", "-1", "1.5", "abc", "1e3", "366"] {
            XCTAssertNil(InviteCreateForm.customExpiryMs(amount: bad, unit: .days), bad)
        }
        XCTAssertNil(InviteCreateForm.customExpiryMs(amount: "8761", unit: .hours))
        XCTAssertEqual(
            InviteCreateForm.customExpiryMs(amount: "١٢", unit: .days), 12 * day,
            "an Arabic number pad's digits")
    }

    func testCustomUsesAcceptsOneToTenThousandAndOneIsSingleUse() {
        XCTAssertEqual(InviteCreateForm.customMaxUsesWire(count: "1"), 0)
        XCTAssertEqual(InviteCreateForm.customMaxUsesWire(count: "25"), 25)
        XCTAssertEqual(InviteCreateForm.customMaxUsesWire(count: "10000"), 10_000)
        for bad in ["", "0", "-3", "2.5", "x", "10001"] {
            XCTAssertNil(InviteCreateForm.customMaxUsesWire(count: bad), bad)
        }
    }

    func testAnInvalidCustomFieldDisablesCreateAndOnlyTypedTextShowsTheError() {
        var form = InviteCreateForm()
        form.expiry = .custom
        XCTAssertNil(form.params, "an empty period disables Create")
        XCTAssertFalse(form.showsCustomExpiryError, "an empty period shows no error")
        form.customExpiryAmount = "400"
        XCTAssertNil(form.params)
        XCTAssertTrue(form.showsCustomExpiryError)
        form.customExpiryUnit = .hours
        XCTAssertFalse(form.showsCustomExpiryError)
        XCTAssertEqual(form.params?.expiry, .fromNow(ms: 400 * hour))

        form.uses = .custom
        XCTAssertNil(form.params)
        XCTAssertFalse(form.showsCustomUsesError)
        form.customUses = "0"
        XCTAssertTrue(form.showsCustomUsesError)
        form.customUses = "42"
        XCTAssertFalse(form.showsCustomUsesError)
        XCTAssertEqual(form.params?.maxUses, 42)
    }

    func testTheLabelIsTrimmedAndClampedToTheCoreLimit() {
        var form = InviteCreateForm()
        form.label = "  Team  "
        XCTAssertEqual(form.params?.label, "Team")
        let long = String(repeating: "ж", count: 40) // 80 UTF-8 bytes
        let clamped = InviteCreateForm.clampLabel(long)
        XCTAssertEqual(clamped.utf8.count, 64)
        XCTAssertEqual(clamped, String(repeating: "ж", count: 32))
        XCTAssertEqual(InviteCreateForm.clampLabel("short"), "short")
        form.label = String(repeating: "a", count: 65)
        XCTAssertNil(form.params, "a label over the limit never reaches the core")
    }

    // MARK: - Expiry rounding of a row

    func testRemainingRoundsToTheNearestUnit() {
        XCTAssertEqual(InviteLinkRows.remaining(6.9 * 86_400), .days(7))
        XCTAssertEqual(InviteLinkRows.remaining(7 * 86_400 - 3_600), .days(7), "made an hour ago")
        XCTAssertEqual(InviteLinkRows.remaining(23.6 * 3_600), .days(1))
        XCTAssertEqual(InviteLinkRows.remaining(23.4 * 3_600), .hours(23))
        XCTAssertEqual(InviteLinkRows.remaining(59.5 * 60), .hours(1))
        XCTAssertEqual(InviteLinkRows.remaining(30), .minutes(1))
        XCTAssertEqual(InviteLinkRows.remaining(59 * 60), .minutes(59))
        XCTAssertEqual(InviteLinkRows.remaining(90 * 60), .hours(2), "half rounds up")
        XCTAssertEqual(InviteLinkRows.remaining(1.5 * 86_400), .days(2))
    }

    func testExpiryCopyOfASevenDayLinkMadeAnHourAgo() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(
            InviteLinkRows.expiryCopy(
                expiryMs: 1_800_000_000_000 + UInt64(7 * day - hour), now: now),
            "Expires in 7d")
    }
}
