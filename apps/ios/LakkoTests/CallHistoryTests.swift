// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/navigation.md` sections 5 and 6.2 — the Calls destination and the
/// flat index behind it.
final class CallHistoryTests: XCTestCase {

    private var calendar: Calendar = {
        var c = Calendar(identifier: .gregorian)
        // A zone well away from UTC, so "which day is this call on" is a real
        // question rather than one that happens to agree.
        c.timeZone = TimeZone(identifier: "Europe/Berlin") ?? .gmt
        return c
    }()

    private let locale = Locale(identifier: "en_GB")

    private func entry(
        _ id: String,
        conversation: String = "aa",
        kind: CallHistoryKind = .chat,
        direction: CallTranscriptRow.Direction = .outgoing,
        outcome: CallHistoryOutcome = .answered,
        video: Bool = false,
        at ms: Int64,
        durationMs: Int64 = 0
    ) -> CallHistoryEntry {
        CallHistoryEntry(
            id: id, conversationId: conversation, kind: kind, direction: direction,
            outcome: outcome, video: video, startedAtMs: ms, durationMs: durationMs)
    }

    /// Midday on a given date, in the viewer's zone.
    private func ms(_ year: Int, _ month: Int, _ day: Int, _ hour: Int = 12, _ minute: Int = 0)
        -> Int64
    {
        var parts = DateComponents()
        parts.year = year
        parts.month = month
        parts.day = day
        parts.hour = hour
        parts.minute = minute
        guard let date = calendar.date(from: parts) else { return 0 }
        return Int64(date.timeIntervalSince1970 * 1000)
    }

    // MARK: - Order

    /// Flat and cross-conversation: a person looking for a call remembers
    /// roughly when, not which thread it belonged to.
    func testNewestFirstAcrossConversations() {
        let rows = CallHistory.ordered([
            entry("a", conversation: "aa", at: 100),
            entry("b", conversation: "bb", at: 300),
            entry("c", conversation: "aa", at: 200),
        ])
        XCTAssertEqual(rows.map(\.id), ["b", "c", "a"])
    }

    /// Two calls in the same millisecond would otherwise sit in whatever order
    /// the store yielded, and a list that reshuffles on a refresh reads as a
    /// bug and is impossible to report.
    func testTheOrderIsTotalSoARefreshNeverReshuffles() {
        let a = entry("a", at: 100)
        let b = entry("b", at: 100)
        XCTAssertEqual(CallHistory.ordered([a, b]).map(\.id), ["a", "b"])
        XCTAssertEqual(CallHistory.ordered([b, a]).map(\.id), ["a", "b"])
    }

    func testTheListIsCapped() {
        let rows = (0..<10).map { entry("\($0)", at: Int64($0)) }
        XCTAssertEqual(CallHistory.ordered(rows, limit: 3).count, 3)
        XCTAssertEqual(CallHistory.ordered(rows, limit: 3).map(\.id), ["9", "8", "7"])
    }

    // MARK: - Days

    func testRunsOfTheSameDayBecomeOneSection() {
        let rows = CallHistory.ordered([
            entry("a", at: ms(2026, 9, 23, 9)),
            entry("b", at: ms(2026, 9, 23, 18)),
            entry("c", at: ms(2026, 9, 22, 10)),
        ])
        let days = CallHistory.byDay(rows, calendar: calendar)
        XCTAssertEqual(days.map(\.day), ["2026-09-23", "2026-09-22"])
        XCTAssertEqual(days.first?.rows.map(\.id), ["b", "a"])
    }

    /// The day is the VIEWER's: a call at 00:30 local is today to the person
    /// who made it, and a list that files it under yesterday because UTC says
    /// so is wrong in a way nobody would think to report.
    func testTheDayIsTheViewersRatherThanUtc() {
        // 00:30 on the 23rd in Berlin is 22:30 on the 22nd in UTC.
        let justAfterMidnight = ms(2026, 9, 23, 0, 30)
        XCTAssertEqual(
            CallHistory.localDay(ms: justAfterMidnight, calendar: calendar), "2026-09-23")
        var utc = calendar
        utc.timeZone = .gmt
        XCTAssertEqual(CallHistory.localDay(ms: justAfterMidnight, calendar: utc), "2026-09-22")
    }

    func testTheThreeHeadings() {
        guard let now = calendar.date(from: DateComponents(year: 2026, month: 9, day: 23, hour: 12))
        else { return XCTFail("could not build the clock") }
        XCTAssertEqual(
            CallHistory.dayLabel("2026-09-23", now: now, calendar: calendar, locale: locale),
            "Today")
        XCTAssertEqual(
            CallHistory.dayLabel("2026-09-22", now: now, calendar: calendar, locale: locale),
            "Yesterday")
        // Older in the same year: a date, and no year on it.
        let older = CallHistory.dayLabel(
            "2026-03-04", now: now, calendar: calendar, locale: locale)
        XCTAssertTrue(older.contains("March"), older)
        XCTAssertFalse(older.contains("2026"), older)
        // Another year names it.
        let lastYear = CallHistory.dayLabel(
            "2025-03-04", now: now, calendar: calendar, locale: locale)
        XCTAssertTrue(lastYear.contains("2025"), lastYear)
    }

    func testAnUnreadableDayKeyIsShownRatherThanCrashingTheScreen() {
        let now = Date()
        XCTAssertEqual(
            CallHistory.dayLabel("nonsense", now: now, calendar: calendar, locale: locale),
            "nonsense")
    }

    // MARK: - The parts, not the sentence

    /// The index carries the parts and the words come back out of
    /// ``CallTranscriptRow`` — the same type the conversation renders — so the
    /// two surfaces can never word one call two ways.
    func testTheWordsComeFromTheConversationsOwnRow() {
        let answered = entry("a", at: 0, durationMs: 74_000)
        XCTAssertEqual(
            answered.transcriptRow,
            CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 74_000)))
        XCTAssertEqual(l10nPlain(answered.transcriptRow.text), "Outgoing call, 1:14")

        let missed = entry("b", direction: .incoming, outcome: .missed, at: 0)
        XCTAssertEqual(missed.transcriptRow.text, "Missed call")

        let declined = entry("c", direction: .incoming, outcome: .declined, at: 0)
        XCTAssertEqual(declined.transcriptRow.text, "Declined")
    }

    /// The round trip that keeps the index honest: a finished row goes in as
    /// parts and comes back out as the identical row.
    func testARowSplitsIntoPartsAndBackWithoutChanging() {
        let rows = [
            CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 5_000)),
            CallTranscriptRow(direction: .incoming, outcome: .answered(durationMs: 0)),
            CallTranscriptRow(direction: .incoming, outcome: .missed),
            CallTranscriptRow(direction: .outgoing, outcome: .declined),
        ]
        for row in rows {
            let (outcome, durationMs) = CallHistoryEntry.outcome(of: row)
            let stored = entry(
                "x", direction: row.direction, outcome: outcome, at: 1, durationMs: durationMs)
            XCTAssertEqual(stored.transcriptRow, row)
        }
    }

    func testTheSecondLineCarriesTheOutcomeAndTheTime() {
        let row = entry("a", at: ms(2026, 9, 23, 12, 4), durationMs: 74_000)
        XCTAssertEqual(
            l10nPlain(CallHistory.rowSubtitle(row, calendar: calendar, locale: locale)),
            "Outgoing call, 1:14, 12:04")
    }

    /// A missed or declined call is the one outcome the reader may want to act
    /// on, so the row carries the danger tint.
    func testOnlyAnUnansweredCallIsTinted() {
        XCTAssertFalse(entry("a", outcome: .answered, at: 0).isUnanswered)
        XCTAssertTrue(entry("b", outcome: .missed, at: 0).isUnanswered)
        XCTAssertTrue(entry("c", outcome: .declined, at: 0).isUnanswered)
    }

    func testTheDirectionGlyphSaysWhichWayTheCallWent() {
        XCTAssertEqual(
            CallHistory.directionGlyph(entry("a", direction: .outgoing, at: 0)),
            "phone.arrow.up.right")
        XCTAssertEqual(
            CallHistory.directionGlyph(entry("b", direction: .incoming, at: 0)),
            "phone.arrow.down.left")
        XCTAssertEqual(
            CallHistory.directionGlyph(entry("c", direction: .outgoing, outcome: .missed, at: 0)),
            "phone.down")
    }

    // MARK: - Accessibility

    /// A duration is announced as a duration, never read digit by digit, and
    /// the row does not promise what the button beside it does.
    func testTheSpokenRowNamesTheConversationAndTheDuration() {
        let row = entry("a", at: ms(2026, 9, 23, 12, 4), durationMs: 74_000)
        let spoken = CallHistory.rowAccessibilityLabel(
            row, title: "Anna", calendar: calendar, locale: locale)
        XCTAssertEqual(
            l10nPlain(spoken), "Anna, Call, outgoing, lasted 1 minute 14 seconds, 12:04")
        XCTAssertFalse(spoken.contains("Call back"))
    }

    func testTheCallBackButtonSaysWhichModeItWillUse() {
        XCTAssertEqual(
            CallHistory.callBackAccessibilityLabel(entry("a", video: false, at: 0)), "Call back")
        XCTAssertEqual(
            CallHistory.callBackAccessibilityLabel(entry("b", video: true, at: 0)),
            "Call back with video")
    }

    // MARK: - The store

    /// A store over an in-memory backing: the sealed backing needs a Keychain
    /// entitlement a unit test does not have, and what these prove is the
    /// store's behaviour, not AES.
    @MainActor
    private func freshStore() -> (CallHistoryStore, CallHistoryBacking) {
        let backing = InMemoryCallHistoryBacking()
        return (CallHistoryStore(backing: backing), backing)
    }

    @MainActor
    func testARecordedCallSurvivesARelaunch() {
        let (store, backing) = freshStore()
        store.bind(accountIdB64: "acct")
        store.record(entry("a", at: 10))
        store.record(entry("b", at: 20))
        XCTAssertEqual(store.entries.map(\.id), ["b", "a"])

        let reopened = CallHistoryStore(backing: backing)
        reopened.bind(accountIdB64: "acct")
        XCTAssertEqual(reopened.entries.map(\.id), ["b", "a"])
    }

    /// One call, one row: the same call recorded twice (a retry, a second pass
    /// over the slot) replaces rather than duplicates.
    @MainActor
    func testRecordingTheSameCallTwiceLeavesOneRow() {
        let (store, _) = freshStore()
        store.bind(accountIdB64: "acct")
        store.record(entry("a", outcome: .missed, at: 10))
        store.record(entry("a", outcome: .answered, at: 10, durationMs: 5_000))
        XCTAssertEqual(store.entries.count, 1)
        XCTAssertEqual(store.entries.first?.outcome, .answered)
    }

    /// Two accounts on one device do not see each other's calls.
    @MainActor
    func testOneAccountsCallsAreNotAnothers() {
        let (store, backing) = freshStore()
        store.bind(accountIdB64: "anna")
        store.record(entry("a", at: 10))
        store.bind(accountIdB64: "boris")
        XCTAssertTrue(store.entries.isEmpty)

        let annas = CallHistoryStore(backing: backing)
        annas.bind(accountIdB64: "anna")
        XCTAssertEqual(annas.entries.map(\.id), ["a"])
    }

    /// A call cannot finish before a session exists, so a write with no
    /// account is a bug rather than a case to guess at — and guessing would
    /// file the row under the wrong person.
    @MainActor
    func testAnUnboundStoreFilesNothing() {
        let (store, _) = freshStore()
        store.record(entry("a", at: 10))
        XCTAssertTrue(store.entries.isEmpty)
    }

    @MainActor
    func testForgettingTheDeviceForgetsTheCalls() {
        let (store, backing) = freshStore()
        store.bind(accountIdB64: "acct")
        store.record(entry("a", at: 10))
        store.clear()
        XCTAssertTrue(store.entries.isEmpty)

        let reopened = CallHistoryStore(backing: backing)
        reopened.bind(accountIdB64: "acct")
        XCTAssertTrue(reopened.entries.isEmpty)
    }

    /// The index is a convenience over history that also lives in the
    /// conversations, so losing it costs a screen and never a record.
    @MainActor
    func testAnUnreadableBlobReadsAsNoCallsRatherThanFailing() {
        let backing = InMemoryCallHistoryBacking()
        backing.save(accountIdB64: "acct", data: Data([0xDE, 0xAD]))
        let store = CallHistoryStore(backing: backing)
        store.bind(accountIdB64: "acct")
        XCTAssertTrue(store.entries.isEmpty)
    }

    /// The index gathers into one place which conversations this person called
    /// and when - the social graph over time, which is what invariant #5 keeps
    /// off the server. So the store built the ordinary way seals it, like the
    /// transcript rows it mirrors and like the same index on Android; a store
    /// that defaulted to a plaintext backing would protect one copy of a fact
    /// and not the other.
    @MainActor
    func testTheOrdinaryStoreSealsWhatItWrites() {
        XCTAssertTrue(CallHistoryStore().backingForTesting is SealedCallHistoryBacking)
    }

    @MainActor
    func testTheStoredListIsCappedSoItCannotGrowWithoutBound() {
        let (store, _) = freshStore()
        store.bind(accountIdB64: "acct")
        for i in 0..<(CallHistory.maxRows + 25) {
            store.record(entry("\(i)", at: Int64(i)))
        }
        XCTAssertEqual(store.entries.count, CallHistory.maxRows)
        XCTAssertEqual(store.entries.first?.id, "\(CallHistory.maxRows + 24)")
    }
}
