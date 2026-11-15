// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import NaturalLanguage
import Translation
import XCTest
@testable import Lakko

/// The tags iOS detection hands to Apple Translation need no mapping for
/// Norwegian Bokmal: NaturalLanguage says `nb`, the core keeps `nb`, and
/// Apple's list spells it `nb` (read on the iOS 27.2 simulator, 2026-11-15:
/// `LanguageAvailability().supportedLanguages` held `nb`, no `no`, and no
/// Tagalog or Filipino in any spelling; NaturalLanguage has no Tagalog and
/// read a Tagalog sentence as Indonesian). So no tag reaches Apple as `tl`.
final class TranslationTagTests: XCTestCase {

    func testNorwegianIsNbEverywhere() async {
        XCTAssertEqual(NLLanguage.norwegian.rawValue, "nb")
        XCTAssertEqual(canonicalLanguageTag(tag: "nb"), "nb")
        let detected = MessageLanguageDetector.detect("Jeg har ikke tid i dag, men vi kan snakke i morgen etter jobb.")
        XCTAssertEqual(detected?.tag, "nb")
        if #available(iOS 18.0, *) {
            let ids = await LanguageAvailability().supportedLanguages.map(\.minimalIdentifier)
            XCTAssertFalse(ids.contains("no"), "Apple spells Norwegian Bokmal nb")
            XCTAssertFalse(ids.contains("tl"), "a Tagalog in Apple's list would need a mapping from tl")
        }
    }
}

/// Seen on an iPhone (2026-11-15): Spanish showed `Spanish · Translate` while
/// Italian, German, Ukrainian and Dutch showed the name alone, which means the
/// engine answered `.unsupported` for those pairs. Apple's answer for each
/// pair, into the interface languages a person is likely to have, as the
/// engine wrapper forms it (`Locale.Language(identifier:)` on the canonical
/// tags). Skipped where the system answers `unsupported` even for English
/// into Spanish: the iOS 27.2 simulator did on 2026-11-15 for every pair it
/// was asked, bare and regional tags alike (es>en, es-ES>en-US, it-IT>en-US,
/// en-US>es-ES, and `status(for:to:)` on whole sentences), while listing 38
/// supported languages, so there is no translation service to ask there and
/// the test means something only on a device.
final class TranslationAvailabilityTests: XCTestCase {

    @MainActor
    func testCommonPairsAreNotUnsupported() async throws {
        guard #available(iOS 18.0, *) else { return }
        let engine = AppleTranslationEngine()
        if await engine.state(source: "en", target: "es") == .unavailable {
            throw XCTSkip("this system answers unsupported even for en>es")
        }
        var report: [String] = []
        for target in ["en", "ru"] {
            for source in ["es", "it", "de", "uk", "nl", "fr"] where source != target {
                let state = await engine.state(source: source, target: target)
                report.append("\(source)>\(target)=\(state)")
                XCTAssertNotEqual(state, .unavailable, "\(source)>\(target)")
            }
        }
        print("translation availability: " + report.joined(separator: " "))
    }
}
