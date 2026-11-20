// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `Translate` and the language line take their source from one rule
/// (``MessageTranslationCenter/resolve(messageChoice:candidates:languages:target:translatable:)``,
/// the core's `resolveTranslationSource`), so they never name different
/// languages; the picker opens only when the rule names none.
@MainActor
final class TranslationSourceTests: XCTestCase {

    private func guess(_ tag: String, _ permille: UInt16, _ alternatives: [String] = []) -> LanguageCandidates {
        LanguageCandidates(detected: DetectedLanguage(tag: tag, confidencePermille: permille), alternatives: alternatives)
    }

    private var confident: UInt16 { translationDetectionConfidentPermille() }

    private func resolve(
        choice: String? = nil, _ candidates: LanguageCandidates?, remembered: String? = nil,
        detected: [(Int64, String)] = [], target: String = "en", translatable: [String]
    ) -> TranslationSourceResolution {
        var languages = ConversationLanguages(picks: [], detections: [])
        if let remembered {
            languages = conversationLanguagesWithPick(languages: languages, tag: remembered, atMs: 1)
        }
        for (id, tag) in detected {
            languages = conversationLanguagesWithDetection(languages: languages, messageId: id, tag: tag)
        }
        return MessageTranslationCenter.resolve(
            messageChoice: choice, candidates: candidates, languages: languages, target: target,
            translatable: translatable)
    }

    func testG55TwoNamesHolaSpanishWhateverOrderTheChatWasDetectedIn() {
        // Seen on a Pixel: one launch said Spanish, the next Italian. The
        // chat holds one confident message each in es, it, uk, de, ru:
        // Spanish and Italian tie, and the detector lists Spanish first.
        let seen: [(Int64, String)] = [(1, "es"), (2, "it"), (3, "uk"), (4, "de"), (5, "ru")]
        // The last order ends on Italian: the order that named it Italian before.
        for order in [seen, Array(seen.reversed()), [seen[1], seen[4], seen[0], seen[3], seen[2]],
                      [seen[0], seen[2], seen[3], seen[4], seen[1]]] {
            let r = resolve(guess("ca", 180, ["es", "fr", "it"]), detected: order, translatable: ["es", "fr", "it"])
            // An unsure guess is never named up front (walk 7, 2026-11-20).
            XCTAssertNil(r.lineTag)
            XCTAssertEqual(r.translateSource, "es")
        }
    }

    func testAUsableGuessBelowTheThresholdTranslatesAtOnceButIsNotNamed() {
        let r = resolve(guess("es", confident - 1), translatable: ["es"])
        XCTAssertEqual(r.translateSource, "es")
        XCTAssertNil(r.lineTag)
        // At the threshold the line names it.
        XCTAssertEqual(resolve(guess("es", confident), translatable: ["es"]).lineTag, "es")
    }

    /// Walk 7 (2026-11-20): English test messages on an iPhone read
    /// "Polish · Translate". Short text is never confident, whatever the
    /// platform detector's probability, so no line names it.
    func testShortTextFromThePlatformDetectorIsNeverNamed() {
        let short = languageCandidatesFromHypotheses(
            text: "W7 web to iphone",
            hypotheses: [LanguageHypothesis(tag: "pl", probabilityPermille: 900)])
        XCTAssertNotNil(short)
        XCTAssertLessThan(short?.detected.confidencePermille ?? 1000, confident)
        XCTAssertNil(resolve(short, translatable: ["pl"]).lineTag)
    }

    func testNoGuessAtAllOpensThePicker() {
        let r = resolve(nil, translatable: ["es"])
        XCTAssertNil(r.translateSource)
        XCTAssertNil(r.lineTag)
    }

    func testTheOrderOfTheSources() {
        // The language chosen for this message wins.
        XCTAssertEqual(resolve(choice: "fr", guess("es", 990), remembered: "de", translatable: ["fr"]).translateSource, "fr")
        // Then a confident detection, over the conversation's language.
        XCTAssertEqual(resolve(guess("es", confident), remembered: "de", translatable: ["es", "de"]).translateSource, "es")
        // An unsure guess the conversation's language is not a candidate of
        // keeps its own name (found live: a remembered Dutch pick read a
        // Ukrainian message from Dutch).
        XCTAssertEqual(resolve(guess("uk", 670), remembered: "nl", translatable: ["uk", "nl"]).translateSource, "uk")
        // With no detection, the conversation's language, and no line.
        let none = resolve(nil, remembered: "de", translatable: ["de"])
        XCTAssertEqual(none.translateSource, "de")
        XCTAssertNil(none.lineTag)
    }

    func testTheHolaCaseNamesAndTranslatesSpanish() {
        // Seen on a Pixel: unsure Catalan, candidates es fr it, no Catalan
        // on the device.
        let r = resolve(guess("ca", 180, ["es", "fr", "it"]), translatable: ["es", "fr", "it"])
        XCTAssertNil(r.lineTag)
        XCTAssertFalse(r.lineCanTranslate)
        XCTAssertEqual(r.translateSource, "es")
    }

    func testAnUnusableGuessYieldsToTheRememberedPickNotThePicker() {
        let r = resolve(guess("tl", 300), remembered: "nl", translatable: ["nl"])
        XCTAssertNil(r.lineTag)
        XCTAssertEqual(r.translateSource, "nl")
        // A guess of the target language is never a source.
        XCTAssertEqual(resolve(guess("en", 300), remembered: "nl", translatable: ["en", "nl"]).translateSource, "nl")
    }

    func testAnUnusableGuessWithNoRememberedPickOpensThePickerMarked() {
        let r = resolve(guess("en", 300), translatable: [])
        XCTAssertNil(r.translateSource)
        XCTAssertEqual(r.guess, "en")
    }

    func testAConfidentLanguageTheDeviceCannotTranslateFromIsNamedAlone() {
        let r = resolve(guess("tk", 950), translatable: ["es"])
        XCTAssertEqual(r.lineTag, "tk")
        XCTAssertFalse(r.lineCanTranslate)
        XCTAssertEqual(r.translateSource, "tk")
    }
}
