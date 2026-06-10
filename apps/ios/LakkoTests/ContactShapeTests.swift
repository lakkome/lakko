// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Tests for `isPlausibleContact` (ADR-0018 factor (d)) — the
/// permissive client-side mirror of the server's `normalize_contact`
/// gate (`services/auth/src/recovery_contact.rs`).
final class ContactShapeTests: XCTestCase {

    // MARK: - Email

    func testAcceptsPlausibleEmail() {
        XCTAssertTrue(isPlausibleContact(kind: .email, contact: "alice@example.com"))
        XCTAssertTrue(isPlausibleContact(kind: .email, contact: "bob+tag@mail.co"))
        // Surrounding whitespace is trimmed first.
        XCTAssertTrue(isPlausibleContact(kind: .email, contact: "  alice@example.com  "))
    }

    func testRejectsMalformedEmail() {
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "no-at-sign"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "@no-local.com"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "no-domain@"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "two@@at.com"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "has space@x.com"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: ""))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "   "))
    }

    func testRejectsOverlongEmail() {
        // > 254 characters is refused (matches the server's <= 254 cap).
        let local = String(repeating: "a", count: 250)
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "\(local)@x.com"))
    }

    // MARK: - Phone

    func testAcceptsPlausiblePhone() {
        XCTAssertTrue(isPlausibleContact(kind: .phone, contact: "+15551234567"))
        // Human separators are stripped before the check.
        XCTAssertTrue(isPlausibleContact(kind: .phone, contact: "+1 (555) 123-4567"))
        XCTAssertTrue(isPlausibleContact(kind: .phone, contact: "+44-20-7946-0958"))
        XCTAssertTrue(isPlausibleContact(kind: .phone, contact: "  +15551234567  "))
    }

    func testRejectsMalformedPhone() {
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "5551234567")) // no leading +
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "+")) // no digits
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "+abc1234")) // non-digit
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "+1234567890123456")) // > 15 digits
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: ""))
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "   "))
    }

    func testRejectsNonAsciiPhoneDigits() {
        // Arabic-Indic digits must be refused (the server only accepts
        // is_ascii_digit after the leading +).
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "+١٢٣٤٥٦"))
    }

    // MARK: - Cross-kind

    func testEmailStringFailsPhoneKindAndViceVersa() {
        // The kind discriminator is load-bearing: an email under the
        // phone kind (no leading +) is rejected, and a phone under the
        // email kind (no @) is rejected.
        XCTAssertFalse(isPlausibleContact(kind: .phone, contact: "alice@example.com"))
        XCTAssertFalse(isPlausibleContact(kind: .email, contact: "+15551234567"))
    }

    // MARK: - ContactKind wire values

    func testContactKindWireValuesMatchServer() {
        // The integer discriminators are wire-format stable and must
        // match the server's ContactKind::as_db (email = 1, phone = 2).
        XCTAssertEqual(ContactKind.email.rawValue, 1)
        XCTAssertEqual(ContactKind.phone.rawValue, 2)
    }
}
