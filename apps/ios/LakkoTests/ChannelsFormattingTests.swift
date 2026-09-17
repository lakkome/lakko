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
            name: "Weather", handle: "weather",
            subscriberCount: 12, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.title, "Weather")
        XCTAssertEqual(row.handleLabel, "@weather")
        XCTAssertEqual(row.subscribersLabel, "12 subscribers")
        XCTAssertEqual(row.secondLine, "@weather \u{00B7} 12 subscribers")
        XCTAssertNil(row.timeLabel)
    }

    /// ADR-0051 §5 — the shared name wins over the local name; the local
    /// name over the handle; the handle over "Channel".
    func testRowTitlePrecedence() {
        XCTAssertEqual(
            ChannelsFormatting.rowModel(
                sharedName: "Lakko News", name: "my local label", handle: "lakkonews",
                subscriberCount: nil, newestPostMs: nil,
                nowMs: now, calendar: calendar, locale: locale).title,
            "Lakko News")
        XCTAssertEqual(
            ChannelsFormatting.rowModel(
                sharedName: " ", name: "my local label", handle: "lakkonews",
                subscriberCount: nil, newestPostMs: nil,
                nowMs: now, calendar: calendar, locale: locale).title,
            "my local label")
        XCTAssertEqual(
            ChannelsFormatting.rowModel(
                name: "  ", handle: "@lakkonews",
                subscriberCount: nil, newestPostMs: nil,
                nowMs: now, calendar: calendar, locale: locale).title,
            "lakkonews", "the bare handle, never 'Channel', when a handle is known")
        XCTAssertEqual(
            ChannelsFormatting.rowModel(
                name: "  ", handle: "",
                subscriberCount: nil, newestPostMs: nil,
                nowMs: now, calendar: calendar, locale: locale).title,
            "Channel")
    }

    /// When the title fell through to the handle the second line must not
    /// repeat it: it carries the count, else the role.
    func testRowNeverShowsTheHandleTwice() {
        let subscribed = ChannelsFormatting.rowModel(
            name: "", handle: "lakkonews", isOwner: false,
            subscriberCount: nil, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(subscribed.title, "lakkonews")
        XCTAssertEqual(subscribed.secondLine, "Subscribed")
        let owned = ChannelsFormatting.rowModel(
            name: "", handle: "lakkonews", isOwner: true,
            subscriberCount: 3, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(owned.secondLine, "3 subscribers")
    }

    func testRowFallsBackToTheRoleWhenNothingElseIsKnown() {
        let reader = ChannelsFormatting.rowModel(
            name: "Weather", handle: "", isOwner: false,
            subscriberCount: nil, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertNil(reader.handleLabel)
        XCTAssertNil(reader.subscribersLabel)
        XCTAssertEqual(reader.secondLine, "Subscribed")
        let owner = ChannelsFormatting.rowModel(
            name: "Weather", handle: "", isOwner: true,
            subscriberCount: nil, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(owner.secondLine, "Your channel")
    }

    func testRowStripsALeadingAtAndSingularisesOneSubscriber() {
        let row = ChannelsFormatting.rowModel(
            name: "News", handle: "@news",
            subscriberCount: 1, newestPostMs: nil,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.handleLabel, "@news")
        XCTAssertEqual(row.subscribersLabel, "1 subscriber")
    }

    func testRowTimeUsesTheChatListVocabulary() {
        let row = ChannelsFormatting.rowModel(
            name: "News", handle: "",
            subscriberCount: nil, newestPostMs: now - 3_600_000,
            nowMs: now, calendar: calendar, locale: locale)
        XCTAssertEqual(row.timeLabel, "11:00")
    }

    /// The reader header's status line: the handle unless the title already
    /// is the handle, then the role.
    func testHeaderStatus() {
        XCTAssertEqual(
            ChannelsFormatting.headerStatus(title: "Lakko News", handle: "lakkonews", isOwner: false),
            "@lakkonews")
        XCTAssertEqual(
            ChannelsFormatting.headerStatus(title: "lakkonews", handle: "@lakkonews", isOwner: false),
            "Subscribed")
        XCTAssertEqual(
            ChannelsFormatting.headerStatus(title: "lakkonews", handle: "lakkonews", isOwner: true),
            "Your channel")
    }

    func testNewestPostPicksTheMaximum() {
        XCTAssertEqual(ChannelsFormatting.newestPostMs([3, 9, 4]), 9)
        XCTAssertNil(ChannelsFormatting.newestPostMs([]))
    }

    func testSearchMatchesTitleHandleAndIgnoresALeadingAt() {
        let row = ChannelsFormatting.rowModel(
            name: "Weather", handle: "wx",
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
