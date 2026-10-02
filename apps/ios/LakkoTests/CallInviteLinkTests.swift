// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/call-links.md` section 3 — which message is a call invitation. The
/// same list as the web `call-invite-link.test.ts`: a message whose whole text
/// is one call link of THIS deployment, and nothing else.
final class CallInviteLinkTests: XCTestCase {

    private let origin = "https://lakko.example"

    private func href(_ text: String) -> String? {
        CallInviteLink.callInviteOf(text, ownOrigin: origin)?.href
    }

    func testAMessageThatIsOneCallLinkOfThisDeploymentIsAnInvitation() {
        XCTAssertEqual(
            href("\(origin)/c/AbC_123-x#secretSECRET12"), "/c/AbC_123-x#secretSECRET12")
        XCTAssertEqual(href("  \(origin)/c/abc#def \n"), "/c/abc#def")
    }

    func testALinkToAnotherHostStaysPlainText() {
        XCTAssertNil(href("https://lakko.example.evil.test/c/abc#def"))
        XCTAssertNil(href("http://lakko.example/c/abc#def"))
        XCTAssertNil(href("https://user@lakko.example/c/abc#def"))
        XCTAssertNil(href("https://lakko.example:8443/c/abc#def"))
        XCTAssertNil(href("https://evil.test/c/abc#https://lakko.example/c/abc"))
    }

    func testOrdinaryTextAndTextAroundALinkAreLeftAlone() {
        XCTAssertNil(href(""))
        XCTAssertNil(href("call me"))
        XCTAssertNil(href("join \(origin)/c/abc#def"))
        XCTAssertNil(href("\(origin)/c/abc#def now"))
        XCTAssertNil(href("\(origin)/c/abc#def\n\(origin)/c/abc#def"))
    }

    func testItRequiresTheCallPathAndTheSecretAfterTheHash() {
        XCTAssertNil(href("\(origin)/c/abc"))
        XCTAssertNil(href("\(origin)/c/abc#"))
        XCTAssertNil(href("\(origin)/c/#def"))
        XCTAssertNil(href("\(origin)/g/abc#def"))
        XCTAssertNil(href("\(origin)/c/abc/extra#def"))
        XCTAssertNil(href("\(origin)/c/abc?x=1#def"))
        XCTAssertNil(href("\(origin)/c/a%20b#def"))
        XCTAssertNil(href("\(origin)/c/abc#de#f"))
    }

    /// `Join` hands the whole link to the app's own `/c/` handling, which
    /// reads the slug from the path and the secret from the fragment: both
    /// must survive.
    @MainActor
    func testTheTargetCarriesTheWholeLinkWithItsFragment() throws {
        let target = try XCTUnwrap(
            CallInviteLink.callInviteOf("\(origin)/c/abc123#secret456", ownOrigin: origin))
        XCTAssertEqual(target.url.path, "/c/abc123")
        XCTAssertEqual(target.url.fragment, "secret456")
        XCTAssertEqual(
            InviteIntake.slug(from: target.url.path, prefix: CallInviteUrl.urlPrefix), "abc123")
    }

    func testTheOriginIsComparedWithoutCaseAndWithoutATrailingSlash() {
        XCTAssertEqual(
            CallInviteLink.callInviteOf(
                "HTTPS://LAKKO.EXAMPLE/c/abc#def", ownOrigin: "https://lakko.example/")?.href,
            "/c/abc#def")
        XCTAssertNil(CallInviteLink.callInviteOf("\(origin)/c/abc#def", ownOrigin: "not an origin"))
    }

    /// The link the app itself mints is one it recognises: the same origin,
    /// the same shape.
    func testALinkThisAppMintsIsRecognised() {
        let minted = "\(CallInviteUrl.defaultOrigin)\(CallInviteUrl.urlPrefix)AbCdEf12#0123456789abcdef"
        XCTAssertEqual(
            CallInviteLink.callInviteOf(minted, ownOrigin: CallInviteUrl.defaultOrigin)?.href,
            "/c/AbCdEf12#0123456789abcdef")
    }
}
