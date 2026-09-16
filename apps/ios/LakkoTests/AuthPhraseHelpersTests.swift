// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the Phase D auth screens
/// (docs/ux/client-redesign.md section 5.6): the phrase grid chunking, the
/// paste parser, and the three-word confirmation picker and checker.
final class AuthPhraseHelpersTests: XCTestCase {

    private let phrase = (1...24).map { "word\($0)" }

    // MARK: - Grid chunking

    func testTwentyFourWordsChunkIntoTwoColumnsOfTwelve() {
        let columns = PhraseWords.columns(phrase)
        XCTAssertEqual(columns.count, 2)
        XCTAssertEqual(columns[0].count, 12)
        XCTAssertEqual(columns[1].count, 12)
        XCTAssertEqual(columns[0].first, PhraseWords.Cell(number: 1, word: "word1"))
        XCTAssertEqual(columns[0].last, PhraseWords.Cell(number: 12, word: "word12"))
        XCTAssertEqual(columns[1].first, PhraseWords.Cell(number: 13, word: "word13"))
        XCTAssertEqual(columns[1].last, PhraseWords.Cell(number: 24, word: "word24"))
    }

    func testOddCountGivesTheFirstColumnTheExtraWord() {
        let columns = PhraseWords.columns(["a", "b", "c", "d", "e"])
        XCTAssertEqual(columns.map(\.count), [3, 2])
        XCTAssertEqual(columns[1].map(\.number), [4, 5])
    }

    func testEmptyPhraseHasNoColumns() {
        XCTAssertEqual(PhraseWords.columns([]), [])
        XCTAssertEqual(PhraseWords.columns(["a"], columns: 0), [])
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

    func testPaddedIsAlwaysTwentyFour() {
        XCTAssertEqual(PhraseWords.padded(["a"]).count, 24)
        XCTAssertEqual(PhraseWords.padded(["a"])[0], "a")
        XCTAssertEqual(PhraseWords.padded(["a"])[1], "")
        XCTAssertEqual(PhraseWords.padded(phrase + ["extra"]).count, 24)
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
            XCTAssertTrue(challenge.positions.allSatisfy { (0..<24).contains($0) })
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
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 11, 23])
        XCTAssertTrue(ThreeWordConfirmation.check(
            challenge: challenge,
            entries: [" Word1 ", "WORD12", "word24\n"],
            words: phrase))
    }

    func testCheckRejectsAWrongWordAndNamesThePosition() {
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 11, 23])
        XCTAssertFalse(ThreeWordConfirmation.check(
            challenge: challenge,
            entries: ["word1", "word13", "word24"],
            words: phrase))
        XCTAssertEqual(
            ThreeWordConfirmation.mismatches(
                challenge: challenge,
                entries: ["word1", "word13", "word24"],
                words: phrase),
            [11])
    }

    func testCheckRejectsMissingEntriesAndAnEmptyPhrase() {
        let challenge = ThreeWordConfirmation.Challenge(positions: [0, 1, 2])
        XCTAssertEqual(
            ThreeWordConfirmation.mismatches(challenge: challenge, entries: ["word1"], words: phrase),
            [1, 2])
        XCTAssertFalse(ThreeWordConfirmation.check(challenge: challenge, entries: ["", "", ""], words: []))
    }
}
