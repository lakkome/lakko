// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `Translate` picks its source language without the picker whenever the
/// detector has any guess; the picker opens only when there is none.
@MainActor
final class TranslationSourceTests: XCTestCase {

    private func detected(_ tag: String, _ permille: UInt16) -> DetectedLanguage {
        DetectedLanguage(tag: tag, confidencePermille: permille)
    }

    private var confident: UInt16 { translationDetectionConfidentPermille() }

    func testAGuessBelowTheThresholdTranslatesAtOnce() {
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: detected("es", confident - 1), conversationChoice: nil),
            .init(tag: "es", isUnsureGuess: true))
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: detected("es", 1), conversationChoice: nil),
            .init(tag: "es", isUnsureGuess: true))
    }

    func testNoGuessAtAllOpensThePicker() {
        XCTAssertNil(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: nil, conversationChoice: nil))
    }

    func testTheOrderOfTheSources() {
        // The language chosen for this message wins.
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: "fr", detected: detected("es", 990), conversationChoice: "de"),
            .init(tag: "fr", isUnsureGuess: false))
        // Then a confident detection, over the conversation's last choice.
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: detected("es", confident), conversationChoice: "de"),
            .init(tag: "es", isUnsureGuess: false))
        // A guess yields to the conversation's last choice.
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: detected("es", confident - 1), conversationChoice: "de"),
            .init(tag: "de", isUnsureGuess: false))
        // With no detection, the conversation's choice.
        XCTAssertEqual(MessageTranslationCenter.translationSource(
            messageChoice: nil, detected: nil, conversationChoice: "de"),
            .init(tag: "de", isUnsureGuess: false))
    }

    func testAnUnsureGuessThatCannotEndOnACorrectableLineOpensThePicker() {
        XCTAssertTrue(MessageTranslationCenter.unsureGuessOpensPicker(guess: "en", target: "en", engine: nil))
        XCTAssertTrue(MessageTranslationCenter.unsureGuessOpensPicker(guess: "en-GB", target: "en", engine: .ready))
        XCTAssertTrue(MessageTranslationCenter.unsureGuessOpensPicker(guess: "tl", target: "en", engine: .unavailable))
        XCTAssertFalse(MessageTranslationCenter.unsureGuessOpensPicker(guess: "es", target: "en", engine: .ready))
        XCTAssertFalse(MessageTranslationCenter.unsureGuessOpensPicker(guess: "es", target: "en", engine: .needsPack))
        XCTAssertFalse(MessageTranslationCenter.unsureGuessOpensPicker(guess: "es", target: "en", engine: nil))
    }
}
