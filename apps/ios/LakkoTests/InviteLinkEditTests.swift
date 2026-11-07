// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/contact-invite-link.md` section 9 — the edit form prefilled from a
/// link's terms, and the two decisions a row reads off its synced record:
/// whether it offers Edit, and whether Delete asks first.
final class InviteLinkEditTests: XCTestCase {
    private let day: UInt64 = 86_400_000
    private let hour: UInt64 = 3_600_000
    private let minted: UInt64 = 1_800_000_000_000

    private func prefill(expiryIn lifetime: UInt64?, maxUses: UInt32 = 0, label: String = "")
        -> InviteCreateForm {
        InviteCreateForm.prefilled(
            label: label, expiryMs: lifetime.map { minted + $0 } ?? 0, maxUses: maxUses,
            mintedAtMs: minted)
    }

    // MARK: - Prefill

    func testTheDefaultLinkReadsBackAsTheDefaultForm() {
        let form = prefill(expiryIn: 7 * day)
        XCTAssertEqual(form, InviteCreateForm())
    }

    func testPresetsMatchWithinAMinute() {
        XCTAssertEqual(prefill(expiryIn: 1 * day).expiry, .oneDay)
        XCTAssertEqual(prefill(expiryIn: 7 * day + 59_000).expiry, .sevenDays)
        XCTAssertEqual(prefill(expiryIn: 30 * day - 60_000).expiry, .thirtyDays)
        // Just past the minute is not the preset.
        XCTAssertEqual(prefill(expiryIn: 7 * day + 61_000).expiry, .custom)
    }

    func testZeroExpiryIsNeverAndNothingElseIs() {
        XCTAssertEqual(prefill(expiryIn: nil).expiry, .never)
        XCTAssertNotEqual(prefill(expiryIn: 365 * day).expiry, .never)
    }

    func testOtherLifetimesAreCustomDaysOrHours() {
        let twelveDays = prefill(expiryIn: 12 * day + 20_000)
        XCTAssertEqual(twelveDays.expiry, .custom)
        XCTAssertEqual(twelveDays.customExpiryUnit, .days)
        XCTAssertEqual(twelveDays.customExpiryAmount, "12")

        let fiveHours = prefill(expiryIn: 5 * hour)
        XCTAssertEqual(fiveHours.expiry, .custom)
        XCTAssertEqual(fiveHours.customExpiryUnit, .hours)
        XCTAssertEqual(fiveHours.customExpiryAmount, "5")

        let thirtyHours = prefill(expiryIn: 30 * hour)
        XCTAssertEqual(thirtyHours.customExpiryUnit, .hours)
        XCTAssertEqual(thirtyHours.customExpiryAmount, "30")

        // Under an hour still reads as the shortest custom period.
        XCTAssertEqual(prefill(expiryIn: 10 * 60_000).customExpiryAmount, "1")
    }

    func testThePrefilledCustomExpiryIsValid() {
        let form = prefill(expiryIn: 12 * day, maxUses: 25)
        XCTAssertEqual(
            form.params,
            .init(label: "", expiry: .fromNow(ms: Int64(12 * day)), maxUses: 25))
    }

    func testUsesReadBack() {
        XCTAssertEqual(prefill(expiryIn: day, maxUses: 0).uses, .one)
        XCTAssertEqual(prefill(expiryIn: day, maxUses: 1).uses, .one)
        XCTAssertEqual(prefill(expiryIn: day, maxUses: 10).uses, .upToTen)
        XCTAssertEqual(prefill(expiryIn: day, maxUses: UInt32.max).uses, .unlimited)
        let custom = prefill(expiryIn: day, maxUses: 42)
        XCTAssertEqual(custom.uses, .custom)
        XCTAssertEqual(custom.customUses, "42")
    }

    func testTheLabelIsKept() {
        XCTAssertEqual(prefill(expiryIn: day, label: "Family").label, "Family")
        XCTAssertEqual(prefill(expiryIn: day, label: "Family").params?.label, "Family")
    }

    // MARK: - Edit and delete decisions

    private func record(
        id: UInt8 = 1, uses: UInt32 = 0, answered: [Data] = [], shared: Bool = false,
        editable: Bool = false, kind: LinkRecordKind = .contact
    ) -> LinkRecord {
        LinkRecord(
            hlc: 0, linkId: Data(repeating: id, count: 16), linkSecret: Data(repeating: 7, count: 12),
            kind: kind, ledgerId: Data(repeating: id, count: 6),
            revocationSeed: Data(repeating: 9, count: 32),
            expiryMs: 0, maxUses: 0, uses: uses, revoked: false,
            mintedAtMs: 1, label: "", answered: answered, shared: shared, editable: editable,
            targetId: Data(), inviteSeed: Data(), capability: Data())
    }

    func testEditIsOfferedOnlyWhenTheRecordSaysSo() {
        XCTAssertTrue(LinkRecords.editable(record(editable: true)))
        XCTAssertFalse(LinkRecords.editable(record(shared: true, editable: false)))
        XCTAssertFalse(LinkRecords.editable(nil))
    }

    func testDeleteAsksFirstUnlessNeverCopiedAndNeverUsed() {
        XCTAssertFalse(LinkRecords.deleteNeedsConfirmation(record(editable: true)))
        XCTAssertTrue(LinkRecords.deleteNeedsConfirmation(record(shared: true)))
        XCTAssertTrue(LinkRecords.deleteNeedsConfirmation(record(uses: 1)))
        XCTAssertTrue(LinkRecords.deleteNeedsConfirmation(record(answered: [Data(repeating: 3, count: 32)])))
        // A link the register does not know might be held by someone.
        XCTAssertTrue(LinkRecords.deleteNeedsConfirmation(nil))
    }

    func testARowFindsItsRecordBySlug() {
        let records = [record(id: 1), record(id: 2)]
        let slug = base64UrlEncodeNoPad(Data(repeating: 2, count: 6))
        XCTAssertEqual(LinkRecords.contactRecord(slug: slug, in: records)?.linkId.first, 2)
        XCTAssertNil(LinkRecords.contactRecord(
            slug: base64UrlEncodeNoPad(Data(repeating: 5, count: 6)), in: records))
        XCTAssertNil(LinkRecords.contactRecord(slug: "%%", in: records))
    }
}
