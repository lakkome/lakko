// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the phrase screens: the row-major grid (3 x 4 at
/// phone width, 4 x 3 on a wide layout), the paste parser, and the
/// three-word confirmation picker and checker of sign-up.
final class AuthPhraseHelpersTests: XCTestCase {

    private let phrase = (1...12).map { "word\($0)" }

    // MARK: - Grid chunking

    func testThePhraseIsTwelveWords() {
        XCTAssertEqual(PhraseWords.count, 12)
    }

    /// Phone width: 3 columns x 4 rows, reading across the rows.
    func testTwelveWordsReadAcrossThreeColumnsAtPhoneWidth() {
        XCTAssertEqual(PhraseWords.gridColumns(wide: false), 3)
        let rows = PhraseWords.rows(phrase, columns: PhraseWords.gridColumns(wide: false))
        XCTAssertEqual(rows.count, 4)
        XCTAssertEqual(rows.map { $0.map(\.number) }, [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]])
        XCTAssertEqual(rows[0].first, PhraseWords.Cell(number: 1, word: "word1"))
        XCTAssertEqual(rows[3].last, PhraseWords.Cell(number: 12, word: "word12"))
    }

    /// Wide layouts (iPad, landscape): 4 columns x 3 rows, the same order.
    func testTwelveWordsReadAcrossFourColumnsOnAWideLayout() {
        XCTAssertEqual(PhraseWords.gridColumns(wide: true), 4)
        let rows = PhraseWords.rows(phrase, columns: PhraseWords.gridColumns(wide: true))
        XCTAssertEqual(rows.map { $0.map(\.number) }, [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]])
    }

    /// Display, confirmation and entry share one order: the flat numbered
    /// list a grid lays out row by row, in either shape.
    func testBothShapesKeepThePhraseOrder() {
        let flat = PhraseWords.numbered(phrase)
        XCTAssertEqual(flat.map(\.number), Array(1...12))
        XCTAssertEqual(flat.map(\.word), phrase)
        for columns in [PhraseWords.narrowColumns, PhraseWords.wideColumns] {
            XCTAssertEqual(PhraseWords.rows(phrase, columns: columns).flatMap { $0 }, flat)
        }
    }

    func testACountThatDoesNotDivideLeavesAShorterLastRow() {
        let rows = PhraseWords.rows(["a", "b", "c", "d", "e"], columns: 3)
        XCTAssertEqual(rows.map(\.count), [3, 2])
        XCTAssertEqual(rows[1].map(\.number), [4, 5])
    }

    func testEmptyPhraseHasNoRowsAndAZeroColumnCountIsTreatedAsOne() {
        XCTAssertEqual(PhraseWords.rows([]), [])
        XCTAssertEqual(PhraseWords.rows(["a", "b"], columns: 0).map { $0.map(\.number) }, [[1], [2]])
    }

    func testWordsOfSplitsOnAnyWhitespace() {
        XCTAssertEqual(PhraseWords.words(of: "  a b\tc\nd  "), ["a", "b", "c", "d"])
    }

    // MARK: - Paste parser

    func testParsePastedStripsNumberingAndSeparators() {
        let pasted = "1. Apple 2) banana\n3: Cherry, 4 delta\n5.echo"
        XCTAssertEqual(
            PhraseWords.parsePasted(pasted),
            ["apple", "banana", "cherry", "delta", "echo"])
    }

    func testParsePastedKeepsAPlainPhrase() {
        XCTAssertEqual(PhraseWords.parsePasted("abandon ability able"), ["abandon", "ability", "able"])
    }

    func testPaddedIsAlwaysTwelve() {
        XCTAssertEqual(PhraseWords.padded(["a"]).count, 12)
        XCTAssertEqual(PhraseWords.padded(["a"])[0], "a")
        XCTAssertEqual(PhraseWords.padded(["a"])[1], "")
        XCTAssertEqual(PhraseWords.padded(phrase + ["extra"]).count, 12)
    }

    // MARK: - Three-word confirmation

    /// A tiny deterministic generator so the pick is reproducible.
    private struct SeededGenerator: RandomNumberGenerator {
        var state: UInt64
        mutating func next() -> UInt64 {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return state
        }
    }

    func testPickReturnsThreeDistinctAscendingPositionsInRange() {
        for seed in 1...50 {
            var generator = SeededGenerator(state: UInt64(seed))
            let challenge = ThreeWordConfirmation.pick(using: &generator)
            XCTAssertEqual(challenge.positions.count, 3)
            XCTAssertEqual(Set(challenge.positions).count, 3)
            XCTAssertEqual(challenge.positions, challenge.positions.sorted())
            XCTAssertTrue(challenge.positions.allSatisfy { (0..<12).contains($0) })
            XCTAssertEqual(challenge.numbers, challenge.positions.map { $0 + 1 })
        }
    }

    func testPickIsDeterministicForAGivenGenerator() {
        var g1 = SeededGenerator(state: 7)
        var g2 = SeededGenerator(state: 7)
        XCTAssertEqual(
            ThreeWordConfirmation.pick(using: &g1),
            ThreeWordConfirmation.pick(using: &g2))
    }

    func testPickNeverAsksForMoreThanThePhraseHolds() {
        var generator = SeededGenerator(state: 1)
        let challenge = ThreeWordConfirmation.pick(total: 2, using: &generator)
        XCTAssertEqual(challenge.positions, [0, 1])
    }

    func testCheckAcceptsCaseAndWhitespaceVariants() {
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 5, 11])
        XCTAssertTrue(ThreeWordConfirmation.check(
            challenge: challenge,
            entries: [" Word1 ", "WORD6", "word12\n"],
            words: phrase))
    }

    func testCheckRejectsAWrongWordAndNamesThePosition() {
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 5, 11])
        XCTAssertFalse(ThreeWordConfirmation.check(
            challenge: challenge,
            entries: ["word1", "word7", "word12"],
            words: phrase))
        XCTAssertEqual(
            ThreeWordConfirmation.mismatches(
                challenge: challenge,
                entries: ["word1", "word7", "word12"],
                words: phrase),
            [5])
    }

    func testCheckRejectsMissingEntriesAndAnEmptyPhrase() {
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 1, 2])
        XCTAssertEqual(
            ThreeWordConfirmation.mismatches(challenge: challenge, entries: ["word1"], words: phrase),
            [1, 2])
        XCTAssertFalse(ThreeWordConfirmation.check(challenge: challenge, entries: ["", "", ""], words: []))
    }

    func testTheCheckStepWaitsForTheAcknowledgement() {
        XCTAssertFalse(ThreeWordConfirmation.canSubmit(entries: ["a", "b", "c"], acknowledged: false))
        XCTAssertTrue(ThreeWordConfirmation.canSubmit(entries: ["a", "b", "c"], acknowledged: true))
    }

    func testTheCheckStepWaitsForAllThreeWords() {
        XCTAssertFalse(ThreeWordConfirmation.canSubmit(entries: ["a", " ", "c"], acknowledged: true))
        XCTAssertFalse(ThreeWordConfirmation.canSubmit(entries: ["a", "b"], acknowledged: true))
        XCTAssertFalse(ThreeWordConfirmation.canSubmit(entries: ["", "", ""], acknowledged: true))
    }

    func testTheNewWordsAfterAResetNeedTheAcknowledgementOnly() {
        XCTAssertFalse(FreshPhraseAcknowledgement.canFinish(words: phrase, acknowledged: false))
        XCTAssertTrue(FreshPhraseAcknowledgement.canFinish(words: phrase, acknowledged: true))
        XCTAssertFalse(FreshPhraseAcknowledgement.canFinish(words: [], acknowledged: true))
    }
}
