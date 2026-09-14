// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `AttachmentRef.name` is chosen by the sender, so these pin that it can
/// never steer a write outside the directory the recipient's Save uses.
final class AttachmentFileNameTests: XCTestCase {
    private let fallback = "attachment-deadbeef"

    func testParentTraversalKeepsOnlyTheLastComponent() {
        XCTAssertEqual(
            AttachmentBridge.safeFileName("../../Library/Preferences/x.plist", fallback: fallback),
            "x.plist"
        )
    }

    func testBackslashIsTreatedAsASeparatorToo() {
        XCTAssertEqual(AttachmentBridge.safeFileName("..\\..\\evil.txt", fallback: fallback), "evil.txt")
    }

    func testDotAndDotDotAreNotNames() {
        XCTAssertEqual(AttachmentBridge.safeFileName("..", fallback: fallback), fallback)
        XCTAssertEqual(AttachmentBridge.safeFileName(".", fallback: fallback), fallback)
        XCTAssertEqual(AttachmentBridge.safeFileName("a/..", fallback: fallback), fallback)
    }

    func testTrailingSeparatorLeavesNothingUsable() {
        XCTAssertEqual(AttachmentBridge.safeFileName("dir/", fallback: fallback), fallback)
    }

    func testControlAndFormatCharactersAreDropped() {
        // U+202E RIGHT-TO-LEFT OVERRIDE renders "gpj.exe" as "exe.jpg".
        XCTAssertEqual(
            AttachmentBridge.safeFileName("photo\u{202E}gpj.exe", fallback: fallback),
            "photogpj.exe"
        )
        XCTAssertEqual(AttachmentBridge.safeFileName("a\u{0000}b\nc", fallback: fallback), "abc")
    }

    func testNilAndBlankFallBack() {
        XCTAssertEqual(AttachmentBridge.safeFileName(nil, fallback: fallback), fallback)
        XCTAssertEqual(AttachmentBridge.safeFileName("   ", fallback: fallback), fallback)
    }

    func testLengthIsCapped() {
        let long = String(repeating: "a", count: 1000)
        XCTAssertEqual(
            AttachmentBridge.safeFileName(long, fallback: fallback).count,
            AttachmentBridge.maxSafeFileNameLength
        )
    }

    func testOrdinaryNamesPassThrough() {
        XCTAssertEqual(AttachmentBridge.safeFileName("Report 2026.pdf", fallback: fallback), "Report 2026.pdf")
        XCTAssertEqual(AttachmentBridge.safeFileName(".hidden", fallback: fallback), ".hidden")
    }
}
