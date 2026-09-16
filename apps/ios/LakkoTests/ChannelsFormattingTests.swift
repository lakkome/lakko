// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the channel row and the post time
/// (docs/ux/client-redesign.md section 5.4).
final class ChannelsFormattingTests: XCTestCase {

    private var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        c.locale = Locale(identifier: "en_GB")
        return c
    }
    private let locale = Locale(identifier: "en_GB")
    /// 2026-09-16 12:00:00 UTC.
    private let now: Int64 = 1_789_560_000_000

    // MARK: Row model

    func testRowJoinsHandleAndSubscriberCount() {
        let row = ChannelsFormatting.rowModel(
            name: "Weather", description: "Daily notes", handle: "weather",
            subscriberCount: 12, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.title, "Weather")
        XCTAssertEqual(row.handleLabel, "@weather")
        XCTAssertEqual(row.subscribersLabel, "12 subscribers")
        XCTAssertEqual(row.secondLine, "@weather \u{00B7} 12 subscribers")
        XCTAssertNil(row.timeLabel)
    }

    func testRowFallsBackToDescriptionWhenNothingElseIsKnown() {
        let row = ChannelsFormatting.rowModel(
            name: "  ", description: "Daily notes", handle: "",
            subscriberCount: nil, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.title, "Channel")
        XCTAssertNil(row.handleLabel)
        XCTAssertNil(row.subscribersLabel)
        XCTAssertEqual(row.secondLine, "Daily notes")
    }

    func testRowStripsALeadingAtAndSingularisesOneSubscriber() {
        let row = ChannelsFormatting.rowModel(
            name: "News", description: "", handle: "@news",
            subscriberCount: 1, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.handleLabel, "@news")
        XCTAssertEqual(row.subscribersLabel, "1 subscriber")
    }

    func testRowTimeUsesTheChatListVocabulary() {
        let row = ChannelsFormatting.rowModel(
            name: "News", description: "", handle: "",
            subscriberCount: nil, newestPostMs: now - 3_600_000,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.timeLabel, "11:00")
    }

    func testNewestPostPicksTheMaximum() {
        XCTAssertEqual(ChannelsFormatting.newestPostMs([3, 9, 4]), 9)
        XCTAssertNil(ChannelsFormatting.newestPostMs([]))
    }

    func testSearchMatchesTitleHandleAndIgnoresALeadingAt() {
        let row = ChannelsFormatting.rowModel(
            name: "Weather", description: "", handle: "wx",
            subscriberCount: nil, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertTrue(ChannelsFormatting.matches(query: "", row: row))
        XCTAssertTrue(ChannelsFormatting.matches(query: "weat", row: row))
        XCTAssertTrue(ChannelsFormatting.matches(query: "@WX", row: row))
        XCTAssertFalse(ChannelsFormatting.matches(query: "sport", row: row))
    }

    // MARK: Relative time

    func testRelativeLabelBands() {
        func label(_ agoS: Int64) -> String {
            ChannelsFormatting.relativeLabel(
                ms: now - agoS * 1000, nowMs: now, calendar: calendar, locale: locale)
        }
        XCTAssertEqual(label(0), "Just now")
        XCTAssertEqual(label(59), "Just now")
        XCTAssertEqual(label(60), "1 min ago")
        XCTAssertEqual(label(5 * 60), "5 min ago")
        XCTAssertEqual(label(2 * 3600), "2 h ago")
        // 12:00 UTC minus 13 h is yesterday 23:00.
        XCTAssertEqual(label(13 * 3600), "Yesterday")
        // The abbreviated month is the OS's ("Sep" or "Sept" for en_GB).
        let sameYear = label(3 * 86_400)
        XCTAssertTrue(sameYear == "13 Sep" || sameYear == "13 Sept", sameYear)
        XCTAssertEqual(label(400 * 86_400), "12 Aug 2025")
    }

    func testRelativeLabelNeverReadsTheFuture() {
        XCTAssertEqual(
            ChannelsFormatting.relativeLabel(
                ms: now + 60_000, nowMs: now, calendar: calendar, locale: locale),
            "Just now")
    }

    // MARK: Access refresh line

    func testAccessRefreshLineIsOnePlainSentence() {
        XCTAssertEqual(
            ChannelsFormatting.accessRefreshLine(
                lastRotationMs: nil, nowMs: now, calendar: calendar, locale: locale),
            "Access refreshes automatically; not refreshed yet.")
        XCTAssertEqual(
            ChannelsFormatting.accessRefreshLine(
                lastRotationMs: 0, nowMs: now, calendar: calendar, locale: locale),
            "Access refreshes automatically; not refreshed yet.")
        XCTAssertEqual(
            ChannelsFormatting.accessRefreshLine(
                lastRotationMs: UInt64(now - 2 * 3_600_000), nowMs: now,
                calendar: calendar, locale: locale),
            "Access last refreshed 2 h ago; refreshes automatically.")
        XCTAssertEqual(
            ChannelsFormatting.accessRefreshLine(
                lastRotationMs: UInt64(now - 10_000), nowMs: now,
                calendar: calendar, locale: locale),
            "Access last refreshed just now; refreshes automatically.")
    }
}
