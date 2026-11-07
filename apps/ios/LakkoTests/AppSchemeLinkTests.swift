// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/desktop.md` section 7.1 — "Open in the app" on a link's web page
/// in Safari opens `lakko://<kind>/<id>#<secret>`; the app reads it as its own
/// `https` link, and nothing else.
final class AppSchemeLinkTests: XCTestCase {
    private let origin = "https://test.lakko.me"
    private let id = "AbCd_-12"
    private let secret = "0123456789abcdef"

    func testEachLedgerKindBecomesThisBuildsLink() throws {
        for kind in ["i", "c", "g"] {
            let url = try XCTUnwrap(URL(string: "lakko://\(kind)/\(id)#\(secret)"))
            XCTAssertEqual(
                InviteIntake.appSchemeLink(url, webOrigin: origin)?.absoluteString,
                "\(origin)/\(kind)/\(id)#\(secret)")
        }
    }

    func testASharedFeedBecomesTheImportLink() throws {
        let url = try XCTUnwrap(URL(string: "lakko://feed/import#AbC_-9"))
        XCTAssertEqual(
            InviteIntake.appSchemeLink(url, webOrigin: origin + "/")?.absoluteString,
            "\(origin)/feed/import#AbC_-9")
    }

    func testAnythingElseIsIgnored() throws {
        for text in [
            "lakko://x/\(id)#\(secret)",
            "lakko://i/short#\(secret)",
            "lakko://i/\(id)",
            "lakko://i/\(id)#short",
            "lakko://i/\(id)/more#\(secret)",
            "lakko://i/\(id)?next=https://evil.example#\(secret)",
            "lakko://c/\(id)#\(secret)%3Cscript%3E",
            "lakko://feed/other#AbC",
            "lakko://feed/import",
            "lakko://settings",
            "https://test.lakko.me/i/\(id)#\(secret)",
        ] {
            let url = try XCTUnwrap(URL(string: text), text)
            XCTAssertNil(InviteIntake.appSchemeLink(url, webOrigin: origin), text)
        }
    }

    @MainActor
    func testTheOpenedSchemeParksTheCallLink() throws {
        let intake = InviteIntake()
        intake.handle(try XCTUnwrap(URL(string: "lakko://c/\(id)#\(secret)")))
        XCTAssertNotNil(intake.pendingCallJoinSlug)
        XCTAssertTrue(intake.pendingCallJoinSlug?.hasPrefix(id) == true)
    }
}
