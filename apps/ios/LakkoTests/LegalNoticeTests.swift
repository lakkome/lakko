// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

// `docs/ux/legal-pages.md` section 2: the Terms of Service and Privacy Policy
// URLs on the deployment's web origin, and the sign-up notice with its two
// link words.

@MainActor
final class LegalNoticeTests: XCTestCase {

    func testPagesAreFixedPathsOnTheGivenOrigin() {
        XCTAssertEqual(LegalPage.terms.url(origin: "https://test.lakko.me")?.absoluteString,
                       "https://test.lakko.me/terms")
        XCTAssertEqual(LegalPage.privacy.url(origin: "https://lakko.me/")?.absoluteString,
                       "https://lakko.me/privacy")
    }

    func testAnEmptyOriginFallsBackToProduction() {
        XCTAssertEqual(LegalPage.terms.url(origin: "  ")?.absoluteString, "https://lakko.me/terms")
    }

    func testAnOriginThatIsNotAUrlGivesNoLink() {
        XCTAssertNil(LegalPage.privacy.url(origin: "not a url"))
    }

    func testTheBuildOriginIsUsedByDefault() {
        XCTAssertEqual(LegalPage.terms.url()?.absoluteString, ApiEndpoints.webOrigin + "/terms")
    }

    private func links(_ text: AttributedString) -> [(String, URL)] {
        text.runs.compactMap { run in
            guard let url = run.link else { return nil }
            return (String(text[run.range].characters), url)
        }
    }

    func testTheNoticeCarriesBothLinksInMessageOrder() {
        let notice = LegalNotice.attributed(
            format: { args in "A \(args["privacy"] ?? "") then \(args["terms"] ?? "")." },
            word: { $0 == .terms ? "Terms" : "Privacy" },
            origin: "https://test.lakko.me"
        )
        XCTAssertEqual(String(notice.characters), "A Privacy then Terms.")
        let found = links(notice)
        XCTAssertEqual(found.map(\.0), ["Privacy", "Terms"])
        XCTAssertEqual(found.map(\.1.absoluteString),
                       ["https://test.lakko.me/privacy", "https://test.lakko.me/terms"])
    }

    func testAMessageWithoutAnArgumentKeepsItsText() {
        let notice = LegalNotice.attributed(
            format: { args in "Only \(args["terms"] ?? "")" },
            word: { $0 == .terms ? "Terms" : "Privacy" },
            origin: "https://lakko.me"
        )
        XCTAssertEqual(String(notice.characters), "Only Terms")
        XCTAssertEqual(links(notice).map(\.0), ["Terms"])
    }

    func testTheEnglishCatalogNotice() {
        let defaults = UserDefaults(suiteName: "LegalNoticeTests-\(UUID().uuidString)")!
        let l10n = Localization(defaults: defaults, bundle: .main, systemPreferred: { ["en-US"] })
        let notice = LegalNotice.attributed(
            format: { l10n.t("legal.signup_notice", texts: $0) },
            word: { l10n.t($0.linkKey) },
            origin: "https://lakko.me"
        )
        let isolates = CharacterSet(charactersIn: "\u{2066}\u{2067}\u{2068}\u{2069}")
        let plain = String(notice.characters).unicodeScalars.filter { !isolates.contains($0) }
        XCTAssertEqual(String(String.UnicodeScalarView(plain)),
                       "By creating an account, you agree to the Terms of Service and the Privacy Policy.")
        XCTAssertEqual(links(notice).map(\.0), ["Terms of Service", "Privacy Policy"])
        XCTAssertEqual(l10n.t(LegalPage.terms.titleKey), "Terms of Service")
        XCTAssertEqual(l10n.t(LegalPage.privacy.titleKey), "Privacy Policy")
    }
}
