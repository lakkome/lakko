// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import XCTest
@testable import Lakko

/// `docs/ux/message-links.md`. The rule for what counts as a link is tested in
/// `lakko_ui_rules::message_links`; here is what this client does with it.
final class MessageLinksTests: XCTestCase {

    private let origin = "https://lakko.me"
    private let id = "AbCd12_-"
    private let secret = "0123456789abcdEF"

    private func open(_ link: String, origin: String? = nil) throws -> MessageLinks.Open {
        MessageLinks.open(try XCTUnwrap(URL(string: link)), ownOrigin: origin ?? self.origin)
    }

    func testEachKindOfLakkoLinkOfThisDeploymentStaysInTheApp() throws {
        for kind in ["i", "c", "g"] {
            XCTAssertEqual(try open("https://lakko.me/\(kind)/\(id)#\(secret)"), .inApp, kind)
        }
    }

    func testALocalOriginWithAPortCounts() throws {
        let local = "http://127.0.0.1:5173"
        XCTAssertEqual(try open("\(local)/i/\(id)#\(secret)", origin: local), .inApp)
    }

    func testTheOriginComparesCaseAsideAndATrailingSlashOfTheOriginIsNothing() throws {
        XCTAssertEqual(try open("HTTPS://Lakko.ME/g/\(id)#\(secret)"), .inApp)
        XCTAssertEqual(try open("https://lakko.me/g/\(id)#\(secret)", origin: "https://lakko.me/"), .inApp)
    }

    func testEveryOtherLinkGoesToTheBrowser() throws {
        let others = [
            "https://example.com/a?b=1",
            "https://example.com/i/\(id)#\(secret)",
            // Same host, another scheme or port: not this deployment.
            "http://lakko.me/i/\(id)#\(secret)",
            "https://lakko.me:8443/i/\(id)#\(secret)",
            "https://lakko.me.evil.example/i/\(id)#\(secret)",
            "https://evil.example@lakko.me/i/\(id)#\(secret)",
            // A shape the intake has no screen for.
            "https://lakko.me/",
            "https://lakko.me/about",
            "https://lakko.me/x/\(id)#\(secret)",
            "https://lakko.me/i/\(id)",
            "https://lakko.me/i/\(id)#",
            "https://lakko.me/i/\(id)#short",
            "https://lakko.me/i/short#\(secret)",
            "https://lakko.me/i/\(id)/#\(secret)",
            "https://lakko.me/i/\(id)/extra#\(secret)",
            "https://lakko.me/i/\(id)?a=1#\(secret)",
            "https://lakko.me/i/\(id)#\(secret)x",
            "https://lakko.me/i/\(id)#\(secret)#\(secret)",
        ]
        for link in others {
            XCTAssertEqual(try open(link), .browser, link)
        }
    }

    func testAnOriginThatIsNotOneMakesNoLinkOurs() throws {
        XCTAssertEqual(try open("https://lakko.me/i/\(id)#\(secret)", origin: ""), .browser)
    }

    @MainActor
    func testAnOwnLinkReachesTheIntakeAsTheScreenItNames() throws {
        let url = try XCTUnwrap(URL(string: "https://lakko.me/i/\(id)#\(secret)"))
        XCTAssertEqual(MessageLinks.open(url, ownOrigin: origin), .inApp)
        let intake = InviteIntake()
        intake.handle(url)
        XCTAssertEqual(intake.pendingSlug, InviteLinkSecret.token(slug: id, fragment: secret))
    }

    func testEveryCharacterOfTheTextSurvives() {
        let texts = [
            "",
            "no link at all",
            "see https://example.com/a?b=1, ok.",
            "(see https://a.example/b).",
            "two https://a.example and https://b.example here\nnext line",
            "مرحبا https://example.com/path اهلا",
        ]
        for text in texts {
            XCTAssertEqual(String(MessageLinks.attributed(text).characters), text)
        }
    }

    func testOnlyTheLinkIsALinkAndItIsUnderlined() throws {
        let text = "see https://example.com/a?b=1, ok."
        let attributed = MessageLinks.attributed(text)
        let linked = attributed.runs.filter { $0.link != nil }
        XCTAssertEqual(linked.count, 1)
        let run = try XCTUnwrap(linked.first)
        XCTAssertEqual(String(attributed[run.range].characters), "https://example.com/a?b=1")
        XCTAssertEqual(run.link, URL(string: "https://example.com/a?b=1"))
        XCTAssertNotNil(attributed[run.range].swiftUI.underlineStyle)
        for plain in attributed.runs where plain.link == nil {
            XCTAssertNil(attributed[plain.range].swiftUI.underlineStyle)
        }
    }

    func testTextWithNoLinkHasNoLinkRun() {
        XCTAssertTrue(MessageLinks.attributed("version 1.2.3 of lakko.me").runs.allSatisfy { $0.link == nil })
    }

    func testAskingTwiceAnswersTheSame() {
        let text = "again https://example.com/twice"
        XCTAssertEqual(MessageLinks.attributed(text), MessageLinks.attributed(text))
    }
}
