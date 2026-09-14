// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0047 §3 / UX spec §4 — emoji search, the Recent row, skin-tone defaults
/// and the one-grapheme reaction check, against the real bundled dataset.
final class EmojiIndexTests: XCTestCase {

    private let dataset = EmojiDataset.bundled
    private var defaults: UserDefaults!
    private var suite: String!

    override func setUp() {
        super.setUp()
        suite = "EmojiIndexTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    func testBundledDatasetLoadsEveryGroupAndResolvesTones() {
        XCTAssertEqual(dataset.groups.first, "Smileys & Emotion")
        XCTAssertFalse(dataset.groups.contains("Component"))
        XCTAssertGreaterThan(dataset.entries.count, 1500)
        XCTAssertEqual(dataset.byEmoji["\u{1F44B}\u{1F3FD}"]?.name, "waving hand")
    }

    func testMalformedRowsAreDroppedNotFatal() {
        let json = Data(#"{"groups":["A"],"emoji":[["x","ok","",0],["y",5,"",0],["z","bad group","",9],"row"]}"#.utf8)
        XCTAssertEqual(EmojiDataset.parse(json).entries.map(\.name), ["ok"])
        XCTAssertTrue(EmojiDataset.parse(Data("not json".utf8)).entries.isEmpty)
    }

    func testSearchRanksNamesFirst() {
        let cats = EmojiIndex.search(dataset, query: "cat")
        XCTAssertEqual(cats.first?.name, "cat")
        XCTAssertTrue(cats.map(\.name).contains("cat face"))
    }

    func testEveryTokenIsRequired() {
        let hits = EmojiIndex.search(dataset, query: "red heart")
        XCTAssertEqual(hits.first?.name, "red heart")
        XCTAssertTrue(hits.allSatisfy { ($0.name + $0.keywords.joined(separator: " ")).contains("red") })
    }

    func testKeywordOnlyMatchAndEmptyQueries() {
        XCTAssertTrue(EmojiIndex.search(dataset, query: "grin").map(\.emoji).contains("\u{1F600}"))
        XCTAssertTrue(EmojiIndex.search(dataset, query: "   ").isEmpty)
        XCTAssertTrue(EmojiIndex.search(dataset, query: "zzqxv").isEmpty)
    }

    func testRecentMovesReuseToFrontAndCapsAt24() {
        var list: [String] = []
        for entry in dataset.entries.prefix(30) { list = EmojiIndex.pushRecent(list, entry.emoji) }
        XCTAssertEqual(list.count, EmojiIndex.maxRecent)
        XCTAssertEqual(EmojiIndex.maxRecent, 24)
        let again = EmojiIndex.pushRecent(list, list[10])
        XCTAssertEqual(again.first, list[10])
        XCTAssertEqual(Set(again).count, 24)
    }

    func testRecentRoundTripsAndIgnoresNonEmoji() {
        EmojiIndex.saveRecent(["\u{1F600}", "\u{1F44D}"], defaults)
        XCTAssertEqual(EmojiIndex.loadRecent(defaults), ["\u{1F600}", "\u{1F44D}"])
        defaults.set(["ok?", "\u{1F600}"], forKey: EmojiIndex.recentKey)
        XCTAssertEqual(EmojiIndex.loadRecent(defaults), ["\u{1F600}"])
        defaults.set("garbage", forKey: EmojiIndex.recentKey)
        XCTAssertEqual(EmojiIndex.loadRecent(defaults), [])
    }

    func testToneBecomesDefaultAndBaseClearsIt() throws {
        let wave = try XCTUnwrap(dataset.byEmoji["\u{1F44B}"])
        let dark = try XCTUnwrap(wave.tones?[4])
        var prefs = EmojiIndex.withTonePref([:], entry: wave, variant: dark)
        XCTAssertEqual(EmojiIndex.displayEmoji(wave, prefs: prefs), dark)
        EmojiIndex.saveTonePrefs(prefs, defaults)
        XCTAssertEqual(EmojiIndex.displayEmoji(wave, prefs: EmojiIndex.loadTonePrefs(defaults)), dark)
        prefs = EmojiIndex.withTonePref(prefs, entry: wave, variant: wave.emoji)
        XCTAssertEqual(EmojiIndex.displayEmoji(wave, prefs: prefs), wave.emoji)
        // A stored variant that is not one of the entry's tones is ignored.
        XCTAssertEqual(EmojiIndex.displayEmoji(wave, prefs: [wave.emoji: "\u{1F600}"]), wave.emoji)
    }

    func testSingleGraphemeAcceptsEveryShapeThePickerProduces() {
        for e in [
            "\u{1F600}",
            "\u{2764}\u{FE0F}",
            "\u{1F44B}\u{1F3FF}",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{1F1FA}\u{1F1F8}",
            "#\u{FE0F}\u{20E3}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            "\u{2139}\u{FE0F}",
        ] {
            XCTAssertTrue(EmojiIndex.isSingleEmojiGrapheme(e), e)
        }
    }

    func testEveryBundledEmojiPasses() {
        let failing = dataset.entries.filter { !EmojiIndex.isSingleEmojiGrapheme($0.emoji) }.map(\.name)
        XCTAssertEqual(failing, [])
        let failingTones = dataset.entries.flatMap { $0.tones ?? [] }.filter { !EmojiIndex.isSingleEmojiGrapheme($0) }
        XCTAssertEqual(failingTones, [])
    }

    func testSingleGraphemeRefusesTextPairsAndFragments() {
        for bad in ["", "a", "ab", "\u{1F600}\u{1F600}", "\u{1F600}a", "1", "\u{1F1FA}", "\u{1F3FB}", "#"] {
            XCTAssertFalse(EmojiIndex.isSingleEmojiGrapheme(bad), bad.debugDescription)
        }
    }

    func testSingleGraphemeRefusesOverTheWireBound() {
        let long = String(repeating: "\u{1F468}\u{200D}", count: 20) + "\u{1F468}"
        XCTAssertGreaterThan(long.utf8.count, EmojiIndex.maxEmojiBytes)
        XCTAssertFalse(EmojiIndex.isSingleEmojiGrapheme(long))
    }
}
