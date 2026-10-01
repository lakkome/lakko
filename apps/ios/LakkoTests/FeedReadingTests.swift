// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/feed.md` sections 4.1 and 5 on iOS — the feed's per-device
/// reading state: the "New" divider, "Show N new", hide from feeds, the
/// channel read markers and counts, the icons, and the item-menu helpers.
/// Mirror of web `feed/feed-marks.test.ts` and Android `FeedReadingTest.kt`.
final class FeedReadingTests: XCTestCase {

    private let groupA = String(repeating: "ab", count: 32)
    private let groupB = String(repeating: "ef", count: 32)
    private let channel = String(repeating: "cd", count: 16)
    private let channel2 = String(repeating: "12", count: 16)

    private func source(_ id: String, _ kind: FeedSource.Kind = .group, title: String = "T") -> FeedSource {
        FeedSource(kind: kind, idHex: id, title: title, muted: false, audience: 3)
    }

    private func slot(cursor: UInt64, posts: [UInt64]) -> ChannelPostSlot {
        ChannelPostSlot(
            cursor: cursor,
            posts: posts.map { PersistedChannelPost(cursor: $0, serverTsMs: 1, text: "p\($0)") })
    }

    // MARK: - "New" divider

    func testDividerIsAboveTheFirstItemNotSeenLastTime() {
        XCTAssertEqual(FeedReading.dividerIndex(itemKeys: ["n1", "a", "b"], previous: ["a", "b"]), 0)
        XCTAssertEqual(FeedReading.dividerIndex(itemKeys: ["a", "n1", "b", "n2"], previous: ["a", "b"]), 1)
    }

    func testNoDividerOnFirstVisitOrWhenNothingIsNew() {
        XCTAssertNil(FeedReading.dividerIndex(itemKeys: ["a", "b"], previous: nil))
        XCTAssertNil(FeedReading.dividerIndex(itemKeys: ["b", "a"], previous: ["a", "b", "c"]))
        XCTAssertNil(FeedReading.dividerIndex(itemKeys: [], previous: ["a"]))
    }

    func testAnEmptyLastLookMakesEverythingNew() {
        XCTAssertEqual(FeedReading.dividerIndex(itemKeys: ["a"], previous: []), 0)
    }

    func testRecordingALookIsPerFeedAndBounded() {
        var r = FeedReading()
        r = r.recordingLooked(feedKey: "latest", itemKeys: ["a", "b"])
        r = r.recordingLooked(feedKey: "custom:abcdefgh", itemKeys: ["c"])
        XCTAssertEqual(r.looked["latest"], ["a", "b"])
        XCTAssertEqual(r.looked["custom:abcdefgh"], ["c"])
        let many = (0..<(FeedReading.maxLookedKeys + 10)).map { "k\($0)" }
        XCTAssertEqual(r.recordingLooked(feedKey: "popular", itemKeys: many).looked["popular"]?.count,
                       FeedReading.maxLookedKeys)
        XCTAssertEqual(r.recordingLooked(feedKey: "bad key!", itemKeys: ["x"]), r)
    }

    // MARK: - "Show N new"

    func testPillCountsWaitingItemsTheListLacks() {
        XCTAssertEqual(FeedReading.pendingNewCount(shown: ["a", "b"], pending: ["c", "a", "d", "b"]), 2)
        XCTAssertEqual(FeedReading.pendingNewCount(shown: ["a", "b"], pending: ["b", "a"]), 0)
        XCTAssertEqual(FeedReading.pendingNewCount(shown: ["a"], pending: nil), 0)
        XCTAssertEqual(FeedReading.pendingNewCount(shown: nil, pending: ["a"]), 0)
    }

    // MARK: - Hide from feeds

    func testHiddenSourcesLeaveEveryFeedBeforeRanking() {
        let r = FeedReading().hiding(groupA).hiding(channel)
        let sources = [source(groupA), source(groupB), source(channel, .channel)]
        XCTAssertEqual(FeedReading.withoutHidden(sources, hidden: r.hidden).map(\.idHex), [groupB])
        XCTAssertEqual(FeedReading.withoutHidden(sources, hidden: []).count, 3)
    }

    func testShowAgainAndIdempotentHide() {
        let r = FeedReading().hiding(groupA).hiding(groupA)
        XCTAssertEqual(r.hidden, [groupA])
        XCTAssertEqual(r.showing(groupA).hidden, [])
        XCTAssertEqual(r.showing(groupB), r)
        XCTAssertEqual(FeedReading().hiding("not hex"), FeedReading())
    }

    func testHiddenListShowsOnlyConversationsStillHeld() {
        let r = FeedReading().hiding(groupB).hiding(groupA).hiding(channel2)
        let held = [source(groupA, title: "A"), source(groupB, title: "B")]
        XCTAssertEqual(FeedReading.hiddenSources(held, hidden: r.hidden).map(\.title), ["B", "A"])
    }

    // MARK: - Channel read state

    func testFreshSubscriptionShowsNoCount() {
        // First sync: no marker yet, so the count is zero ...
        let s = slot(cursor: 30, posts: [10, 20, 30])
        XCTAssertEqual(
            FeedReading.channelUnreadCount(postCursors: [10, 20, 30], newestRead: nil, own: false), 0)
        // ... and the seed puts the marker at the newest post held.
        let seeded = FeedReading().seedingChannelReads([channel: s])
        XCTAssertEqual(seeded.channelRead[channel], 30)
        XCTAssertEqual(
            FeedReading.channelUnreadCount(
                postCursors: [10, 20, 30], newestRead: seeded.channelRead[channel], own: false), 0)
        // Posts after it count.
        XCTAssertEqual(
            FeedReading.channelUnreadCount(
                postCursors: [10, 20, 30, 40, 50], newestRead: seeded.channelRead[channel], own: false), 2)
    }

    func testSeedUsesTheSlotCursorAndLeavesAMarkedChannelAlone() {
        let seeded = FeedReading().seedingChannelReads([channel: slot(cursor: 45, posts: [10, 40])])
        XCTAssertEqual(seeded.channelRead[channel], 45)
        let marked = FeedReading().advancingChannelRead(channel, cursor: 12)
        XCTAssertEqual(marked.seedingChannelReads([channel: slot(cursor: 99, posts: [99])]).channelRead[channel], 12)
    }

    func testOwnChannelCountsZero() {
        XCTAssertEqual(FeedReading.channelUnreadCount(postCursors: [5, 6, 7], newestRead: 1, own: true), 0)
    }

    func testReadingTheNewestResetsTheCountEvenOverSkippedPosts() {
        let r = FeedReading().seedingChannelReads([channel: slot(cursor: 10, posts: [10])])
            .advancingChannelRead(channel, cursor: 50)
        XCTAssertEqual(
            FeedReading.channelUnreadCount(postCursors: [10, 20, 30, 40, 50], newestRead: r.channelRead[channel],
                                           own: false), 0)
    }

    func testChannelReadNeverMovesBack() {
        let r = FeedReading().advancingChannelRead(channel, cursor: 40)
        XCTAssertEqual(r.advancingChannelRead(channel, cursor: 30).channelRead[channel], 40)
        XCTAssertEqual(r.advancingChannelRead(channel, cursor: 41).channelRead[channel], 41)
        XCTAssertEqual(FeedReading().advancingChannelRead(channel, cursor: 0), FeedReading())
        XCTAssertEqual(FeedReading().advancingChannelRead("zz", cursor: 3), FeedReading())
    }

    func testFeedDwellOnAChannelPostWantsOnlyPostsAboveTheMarker() {
        XCTAssertTrue(FeedForward.channelPostWantsSeen(cursor: 5, newestRead: nil))
        XCTAssertTrue(FeedForward.channelPostWantsSeen(cursor: 5, newestRead: 4))
        XCTAssertFalse(FeedForward.channelPostWantsSeen(cursor: 5, newestRead: 5))
        XCTAssertFalse(FeedForward.channelPostWantsSeen(cursor: 0, newestRead: nil))
    }

    // MARK: - Codec

    func testCodecRoundTripsAndDropsMalformedEntries() {
        let r = FeedReading()
            .hiding(groupA)
            .recordingLooked(feedKey: "latest", itemKeys: ["group:x:1"])
            .advancingChannelRead(channel, cursor: 7)
        XCTAssertEqual(FeedReading.decode(r.encode()), r)

        let raw = """
            {"version":1,"hidden":["\(groupA)","nope",""],"looked":{"ok":["a"],"bad key":["b"]},
             "channelRead":{"\(channel)":3,"short":4}}
            """
        let decoded = FeedReading.decode(Data(raw.utf8))
        XCTAssertEqual(decoded.hidden, [groupA])
        XCTAssertEqual(decoded.looked, ["ok": ["a"]])
        XCTAssertEqual(decoded.channelRead, [channel: 3])
        XCTAssertEqual(FeedReading.decode(Data(#"{"version":2,"hidden":[]}"#.utf8)), FeedReading())
        XCTAssertEqual(FeedReading.decode(Data("junk".utf8)), FeedReading())
    }

    // MARK: - Icons

    func testIconsMapTheTwelveInSpecOrderAndUnknownToTheDefault() {
        XCTAssertEqual(FeedIcons.all.count, 12)
        XCTAssertEqual(
            FeedIcons.all.map(\.label),
            ["Feed", "Star", "Heart", "Lightning", "Book", "Code", "Music", "Camera", "Globe",
             "Speech bubble", "Bell", "Flag"])
        XCTAssertEqual(FeedIcons.symbol(0), "newspaper")
        XCTAssertEqual(FeedIcons.symbol(3), "bolt")
        XCTAssertEqual(FeedIcons.symbol(11), "flag")
        XCTAssertEqual(FeedIcons.symbol(12), FeedIcons.symbol(0))
        XCTAssertEqual(FeedIcons.symbol(255), FeedIcons.symbol(0))
        XCTAssertEqual(Set(FeedIcons.all.map(\.symbol)).count, 12)
    }

    func testPresetIcons() {
        XCTAssertEqual(FeedIcons.presetSymbol(.latest), FeedIcons.symbol(0))
        XCTAssertEqual(FeedIcons.presetSymbol(.popular), FeedIcons.symbol(3))
        XCTAssertEqual(FeedIcons.presetSymbol(.forMe), "person")
        XCTAssertEqual(FeedIcons.discoverSymbol, FeedIcons.symbol(8))
    }

    // MARK: - Item menu

    func testChannelForwardCarriesTheChannelNameAndNoOrigin() {
        let p = FeedForward.channelPayload(text: "hello", channelTitle: "  News  ")
        XCTAssertEqual(p.text, "hello")
        XCTAssertEqual(p.forward.author, "News")
        XCTAssertNil(p.forward.originGroupIdHex)
        XCTAssertNil(p.forward.originMessageId)
        XCTAssertNil(FeedForward.channelPayload(text: "x", channelTitle: " ").forward.author)
    }

    func testCopyAndReplyAvailability() {
        let text = FeedMessage(messageId: 4, direction: .incoming, text: "hi", timeMs: 1)
        let blank = FeedMessage(messageId: 4, direction: .incoming, text: "  ", timeMs: 1)
        XCTAssertTrue(FeedForward.canCopy(text))
        XCTAssertFalse(FeedForward.canCopy(blank))
        let groupRef = FeedCandidateRef(source: source(groupA), message: text, mentionsMe: false, repliesToMe: false)
        let channelRef = FeedCandidateRef(
            source: source(channel, .channel), message: text, mentionsMe: false, repliesToMe: false)
        XCTAssertTrue(FeedForward.canReply(groupRef))
        XCTAssertFalse(FeedForward.canReply(channelRef))
    }

    // MARK: - Opening a shared file

    func testFileImportRefusesEmptyTooLargeOrUnreadable() {
        XCTAssertTrue(FeedDraft.importRequest(fileBytes: nil, limit: 10).refused)
        XCTAssertTrue(FeedDraft.importRequest(fileBytes: Data(), limit: 10).refused)
        XCTAssertTrue(FeedDraft.importRequest(fileBytes: Data(count: 11), limit: 10).refused)
        let ok = FeedDraft.importRequest(fileBytes: Data([1, 2, 3]), limit: 10)
        XCTAssertFalse(ok.refused)
        XCTAssertEqual(ok.bytes, Data([1, 2, 3]))
    }

    func testSharedFeedFileIsRecognisedByExtension() {
        XCTAssertTrue(FeedDraft.isSharedFeedFile(URL(fileURLWithPath: "/tmp/My-feed.lakkofeed")))
        XCTAssertTrue(FeedDraft.isSharedFeedFile(URL(fileURLWithPath: "/tmp/x.LAKKOFEED")))
        XCTAssertFalse(FeedDraft.isSharedFeedFile(URL(fileURLWithPath: "/tmp/x.json")))
        XCTAssertFalse(FeedDraft.isSharedFeedFile(URL(string: "https://lakko.me/x.lakkofeed")!))
    }

    func testOpeningAFileThroughTheIntakeParksAnImport() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("feed.lakkofeed")
        try Data([9, 9, 9]).write(to: url)
        let request = FeedDraft.importRequest(fromFile: url)
        XCTAssertFalse(request.refused)
        XCTAssertEqual(request.bytes, Data([9, 9, 9]))
    }
}
