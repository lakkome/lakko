// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Tests for `normalizeTotpCode` (ADR-0018 factor (c)) — the
/// client-side mirror of the server's `parse_code` gate.
final class TotpCodeTests: XCTestCase {

    func testAcceptsExactlySixAsciiDigits() {
        XCTAssertEqual(normalizeTotpCode("123456"), "123456")
        XCTAssertEqual(normalizeTotpCode("000000"), "000000")
    }

    func testTrimsSurroundingWhitespace() {
        XCTAssertEqual(normalizeTotpCode("  123456  "), "123456")
        XCTAssertEqual(normalizeTotpCode("\t123456\n"), "123456")
    }

    func testRejectsWrongLength() {
        XCTAssertNil(normalizeTotpCode("12345"))
        XCTAssertNil(normalizeTotpCode("1234567"))
        XCTAssertNil(normalizeTotpCode(""))
    }

    func testRejectsNonDigits() {
        XCTAssertNil(normalizeTotpCode("12345a"))
        XCTAssertNil(normalizeTotpCode("12 456"))
        XCTAssertNil(normalizeTotpCode("12-456"))
    }

    func testRejectsNonAsciiNumerals() {
        // Arabic-Indic digits must be refused (the server only
        // accepts is_ascii_digit).
        XCTAssertNil(normalizeTotpCode("١٢٣٤٥٦"))
    }
}
