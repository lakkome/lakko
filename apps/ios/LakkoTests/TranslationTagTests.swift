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
