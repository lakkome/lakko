// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0058 S3 — the iOS interface-language runtime: the resolution order
// (stored choice, else the system preference, else English), the per-device
// persistence of the choice, and the real compiled catalog the app bundles.

@MainActor
final class LocalizationTests: XCTestCase {
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "LocalizationTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func make(system: [String]) -> Localization {
        Localization(defaults: defaults, bundle: .main, systemPreferred: { system })
    }

    func testFollowsTheFirstSupportedSystemLanguage() {
        let l10n = make(system: ["de-DE", "ru-RU", "en-US"])
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "ru")
    }

    func testFallsBackToEnglishWhenNoSystemLanguageIsSupported() {
        let l10n = make(system: ["de-DE", "ja-JP"])
        XCTAssertEqual(l10n.language, "en")
        XCTAssertEqual(l10n.t("nav.chats"), "Chats")
    }

    func testStoredChoiceWinsOverTheSystem() {
        defaults.set("uk", forKey: Localization.storageKey)
        let l10n = make(system: ["ru-RU"])
        XCTAssertEqual(l10n.selected, "uk")
        XCTAssertEqual(l10n.language, "uk")
    }

    func testUnknownStoredChoiceReadsAsSameAsSystem() {
        defaults.set("xx", forKey: Localization.storageKey)
        let l10n = make(system: ["es-MX"])
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "es")
    }

    func testSelectPersistsAndSurvivesANewInstance() {
        let first = make(system: ["en-US"])
        first.select("he")
        XCTAssertEqual(defaults.string(forKey: Localization.storageKey), "he")
        XCTAssertEqual(first.language, "he")

        let second = make(system: ["en-US"])
        XCTAssertEqual(second.selected, "he")
        XCTAssertEqual(second.language, "he")
    }

    func testSameAsSystemRemovesTheStoredChoice() {
        let l10n = make(system: ["tr-TR"])
        l10n.select("fa")
        XCTAssertEqual(l10n.language, "fa")
        l10n.select(nil)
        XCTAssertNil(defaults.object(forKey: Localization.storageKey))
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "tr")
    }

    func testRightToLeftLanguagesMirrorTheLayout() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.layoutDirection, .leftToRight)
        for code in ["ar", "fa", "he"] {
            l10n.select(code)
            XCTAssertTrue(l10n.isRightToLeft, code)
            XCTAssertEqual(l10n.layoutDirection, .rightToLeft, code)
        }
        l10n.select("zh-Hans")
        XCTAssertEqual(l10n.layoutDirection, .leftToRight)
    }

    func testTheBundledCatalogTranslatesAndSwitchesLive() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.t("settings.language.title"), "Language")
        l10n.select("ru")
        let russian = l10n.t("settings.language.title")
        XCTAssertFalse(russian.isEmpty)
        XCTAssertNotEqual(russian, "Language")
        XCTAssertNotEqual(russian, "settings.language.title")
        XCTAssertEqual(l10n.locale.identifier, "ru")
    }

    func testFormattedMessageCarriesItsArgument() {
        let l10n = make(system: ["en-US"])
        let text = l10n.t("settings.language.same_as_system_detail", texts: ["language": "Deutsch"])
        XCTAssertTrue(text.hasPrefix("Same as system"))
        XCTAssertTrue(text.contains("Deutsch"))
    }

    func testEveryLanguageHasATranslatedNameAndOwnName() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.languages.count, 11)
        for info in l10n.languages {
            XCTAssertFalse(info.ownName.isEmpty, info.code)
            XCTAssertFalse(l10n.translatedName(of: info).hasPrefix("language.name."), info.code)
        }
    }
}
