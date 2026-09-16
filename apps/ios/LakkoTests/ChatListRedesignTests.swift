// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the Phase B list and timeline
/// (docs/ux/client-redesign.md sections 5.1 and 5.2): the persisted
/// conversation kind, the row time and preview, the date separators, the
/// run grouping and the bubble time.
final class ChatListRedesignTests: XCTestCase {

    // MARK: - Conversation kind

    func testFirstSightDerivesFromPeopleCountAndPersists() {
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: nil, peopleCount: 2),
            .init(kind: .chat, persist: .chat))
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: nil, peopleCount: 3),
            .init(kind: .group, persist: .group))
    }

    func testUnreadableRosterIsShownAsChatButNotPersisted() {
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: nil, peopleCount: nil),
            .init(kind: .chat, persist: nil))
    }

    func testStoredKindWinsOverThePeopleCount() {
        // A group made through the new-group flow with two people stays a group.
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .group, peopleCount: 2),
            .init(kind: .group, persist: nil))
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .group, peopleCount: nil),
            .init(kind: .group, persist: nil))
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .chat, peopleCount: 2),
            .init(kind: .chat, persist: nil))
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .chat, peopleCount: nil),
            .init(kind: .chat, persist: nil))
    }

    func testAChatFlipsToGroupAboveTwoPeopleNeverBack() {
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .chat, peopleCount: 3),
            .init(kind: .group, persist: .group))
        XCTAssertEqual(
            ConversationKindDecision.resolve(stored: .group, peopleCount: 1),
            .init(kind: .group, persist: nil))
    }

    func testStoreIsPerAccountAndOneWay() throws {
        let suite = "ChatListRedesignTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = ConversationKindStore(defaults: defaults)

        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1"))
        store.record(accountIdB64: "a", groupIdHex: "g1", kind: .chat)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), .chat)
        XCTAssertNil(store.load(accountIdB64: "b", groupIdHex: "g1"))

        store.record(accountIdB64: "a", groupIdHex: "g1", kind: .group)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), .group)
        // group -> chat is refused.
        XCTAssertEqual(store.record(accountIdB64: "a", groupIdHex: "g1", kind: .chat), .group)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), .group)

        store.clear(accountIdB64: "a")
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1"))
    }

    // MARK: - Row time

    private var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        calendar.firstWeekday = 2
        return calendar
    }

    /// 2026-09-16 14:05:00 UTC, a Wednesday.
    private let now: Int64 = 1_789_567_500_000
    private let hour: Int64 = 3_600_000
    private let day: Int64 = 86_400_000

    func testRowTimeBuckets() {
        XCTAssertEqual(ChatListFormatting.bucket(ms: now - hour, nowMs: now, calendar: utc), .today)
        XCTAssertEqual(ChatListFormatting.bucket(ms: now - day, nowMs: now, calendar: utc), .thisWeek)
        XCTAssertEqual(ChatListFormatting.bucket(ms: now - 6 * day, nowMs: now, calendar: utc), .thisWeek)
        XCTAssertEqual(ChatListFormatting.bucket(ms: now - 7 * day, nowMs: now, calendar: utc), .sameYear)
        XCTAssertEqual(ChatListFormatting.bucket(ms: now - 400 * day, nowMs: now, calendar: utc), .otherYear)
    }

    func testRowTimeStrings() {
        let gb = Locale(identifier: "en_GB")
        XCTAssertEqual(
            ChatListFormatting.lastMessageTime(ms: now - hour, nowMs: now, calendar: utc, locale: gb),
            "13:05")
        XCTAssertEqual(
            ChatListFormatting.lastMessageTime(ms: now - day, nowMs: now, calendar: utc, locale: gb),
            "Tue")
        // The abbreviated month is the OS's ("Sep" or "Sept" for en_GB).
        let older = ChatListFormatting.lastMessageTime(
            ms: now - 10 * day, nowMs: now, calendar: utc, locale: gb)
        XCTAssertTrue(older == "6 Sep" || older == "6 Sept", older)
        XCTAssertEqual(
            ChatListFormatting.lastMessageTime(ms: now - 400 * day, nowMs: now, calendar: utc, locale: gb),
            "12 Aug 2025")
        // The device's time style is honoured: a 12-hour locale renders AM/PM.
        let us = Locale(identifier: "en_US")
        let usToday = ChatListFormatting.lastMessageTime(
            ms: now - hour, nowMs: now, calendar: utc, locale: us)
        XCTAssertTrue(usToday.hasPrefix("1:05"), usToday)
    }

    // MARK: - Row preview

    func testPreviewPrefixes() {
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "hi", attachmentLabels: [], direction: .outgoing, deleted: false,
                isGroup: false, senderName: nil),
            "You: hi")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "hi", attachmentLabels: [], direction: .incoming, deleted: false,
                isGroup: true, senderName: "Anna"),
            "Anna: hi")
        // No sender prefix in a 1x1, nor when the author has no name.
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "hi", attachmentLabels: [], direction: .incoming, deleted: false,
                isGroup: false, senderName: "Anna"),
            "hi")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "hi", attachmentLabels: [], direction: .incoming, deleted: false,
                isGroup: true, senderName: nil),
            "hi")
    }

    func testPreviewMediaAndDeleted() {
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "", attachmentLabels: ["Photo"], direction: .incoming, deleted: false,
                isGroup: false, senderName: nil),
            "Photo")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "", attachmentLabels: ["Photo", "File"], direction: .outgoing, deleted: false,
                isGroup: false, senderName: nil),
            "You: Photo +1")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "line one\nline two", attachmentLabels: [], direction: .incoming, deleted: false,
                isGroup: false, senderName: nil),
            "line one line two")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "secret", attachmentLabels: [], direction: .incoming, deleted: true,
                isGroup: true, senderName: "Anna"),
            "Anna: Message deleted")
    }

    func testMediaLabelsFollowTheInlineDecision() {
        // A fake allowlist so the test does not depend on the core's table.
        let allowlist: InlineMedia.Allowlist = { mime in
            switch mime {
            case "image/jpeg", "image/gif": return .image
            case "audio/mp4": return .audio
            case "video/mp4": return .video
            default: return nil
            }
        }
        XCTAssertEqual(
            ChatListFormatting.mediaLabel(kind: "voice", mime: "audio/mp4", allowlist: allowlist),
            "Voice message")
        XCTAssertEqual(
            ChatListFormatting.mediaLabel(kind: "video_note", mime: "video/mp4", allowlist: allowlist),
            "Video message")
        XCTAssertEqual(
            ChatListFormatting.mediaLabel(kind: "gif", mime: "image/gif", allowlist: allowlist), "GIF")
        XCTAssertEqual(
            ChatListFormatting.mediaLabel(kind: nil, mime: "image/jpeg", allowlist: allowlist), "Photo")
        // A sender-chosen kind never overrides a refused MIME.
        XCTAssertEqual(
            ChatListFormatting.mediaLabel(kind: "voice", mime: "text/html", allowlist: allowlist), "File")
        XCTAssertEqual(ChatListFormatting.mediaLabel(kind: nil, mime: nil, allowlist: allowlist), "File")
    }

    func testSearchMatchesNameOrPreviewCaseInsensitively() {
        XCTAssertTrue(ChatListFormatting.matches(query: "", title: "Anna", preview: "hi"))
        XCTAssertTrue(ChatListFormatting.matches(query: "ann", title: "Anna", preview: "hi"))
        XCTAssertTrue(ChatListFormatting.matches(query: "HI", title: "Anna", preview: "You: hi"))
        XCTAssertFalse(ChatListFormatting.matches(query: "bob", title: "Anna", preview: "hi"))
    }

    // MARK: - Timeline

    private func message(
        _ id: Int64, _ direction: Direction, author: String?, atMs: Int64?
    ) -> ChatMessage {
        ChatMessage(
            messageId: id,
            direction: direction,
            text: "m\(id)",
            createdAt: atMs.map { String($0) } ?? "",
            authorKeyB64: author)
    }

    /// The test's `createdAt` is the millisecond stamp as decimal text.
    private func parse(_ raw: String) -> Int64? { Int64(raw) }

    func testDateLabels() {
        let gb = Locale(identifier: "en_GB")
        XCTAssertEqual(ChatTimeline.dateLabel(ms: now - hour, nowMs: now, calendar: utc, locale: gb), "Today")
        XCTAssertEqual(
            ChatTimeline.dateLabel(ms: now - day, nowMs: now, calendar: utc, locale: gb), "Yesterday")
        XCTAssertEqual(
            ChatTimeline.dateLabel(ms: now - 10 * day, nowMs: now, calendar: utc, locale: gb),
            "6 September")
        XCTAssertEqual(
            ChatTimeline.dateLabel(ms: now - 400 * day, nowMs: now, calendar: utc, locale: gb),
            "12 August 2025")
    }

    func testBubbleTimeFollowsTheLocaleStyle() {
        XCTAssertEqual(
            ChatTimeline.timeLabel(ms: now, calendar: utc, locale: Locale(identifier: "en_GB")), "14:05")
        // A 12-hour locale renders AM/PM (the OS separates it with a narrow
        // no-break space, so compare the pieces rather than the bytes).
        let us = ChatTimeline.timeLabel(ms: now, calendar: utc, locale: Locale(identifier: "en_US"))
        XCTAssertTrue(us.hasPrefix("2:05") && us.hasSuffix("PM"), us)
    }

    func testRunsGroupOneSenderWithinFiveMinutes() {
        let minute: Int64 = 60_000
        let messages = [
            message(1, .incoming, author: "anna", atMs: now),
            message(2, .incoming, author: "anna", atMs: now + 2 * minute),
            message(3, .incoming, author: "anna", atMs: now + 8 * minute),  // > 5 min after 2
            message(4, .incoming, author: "ivan", atMs: now + 8 * minute),  // other sender
            message(5, .outgoing, author: "me", atMs: now + 9 * minute),
            message(6, .outgoing, author: "me", atMs: now + 9 * minute + 1),
        ]
        let entries = ChatTimeline.layout(
            messages: messages, nowMs: now, calendar: utc, locale: Locale(identifier: "en_GB"),
            parseMs: parse)
        XCTAssertEqual(entries.map(\.startsRun), [true, false, true, true, true, false])
        XCTAssertEqual(entries.map(\.endsRun), [false, true, true, true, false, true])
        XCTAssertEqual(entries.map(\.timeLabel), ["14:05", "14:07", "14:13", "14:13", "14:14", "14:14"])
    }

    func testDateSeparatorOpensEachDayAndBreaksARun() {
        let messages = [
            message(1, .incoming, author: "anna", atMs: now - day),
            message(2, .incoming, author: "anna", atMs: now - day + 1000),
            message(3, .incoming, author: "anna", atMs: now),
        ]
        let entries = ChatTimeline.layout(
            messages: messages, nowMs: now, calendar: utc, locale: Locale(identifier: "en_GB"),
            parseMs: parse)
        XCTAssertEqual(entries.map(\.dateLabel), ["Yesterday", nil, "Today"])
        XCTAssertEqual(entries.map(\.startsRun), [true, false, true])
        XCTAssertEqual(entries.map(\.endsRun), [false, true, true])
    }

    func testSystemRowsAndUntimedMessagesNeverJoinARun() {
        let messages = [
            message(1, .incoming, author: "anna", atMs: now),
            message(2, .system, author: nil, atMs: now + 1000),
            message(3, .incoming, author: "anna", atMs: now + 2000),
            message(4, .incoming, author: "anna", atMs: nil),
        ]
        let entries = ChatTimeline.layout(
            messages: messages, nowMs: now, calendar: utc, locale: Locale(identifier: "en_GB"),
            parseMs: parse)
        XCTAssertEqual(entries.map(\.startsRun), [true, true, true, true])
        XCTAssertNil(entries[3].timeLabel)
        XCTAssertNil(entries[3].dateLabel)
    }

    func testProtocolBookkeepingRowsAreHidden() {
        XCTAssertTrue(
            ChatTimeline.isHiddenSystemRow(
                ChatMessage(messageId: 1, direction: .system, text: "[mls commit]", createdAt: "")))
        XCTAssertFalse(
            ChatTimeline.isHiddenSystemRow(
                ChatMessage(messageId: 1, direction: .incoming, text: "[mls commit]", createdAt: "")))
        XCTAssertFalse(
            ChatTimeline.isHiddenSystemRow(
                ChatMessage(messageId: 1, direction: .system, text: "Anna joined", createdAt: "")))
    }
}
