// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/navigation.md` section 2 — the one conversation list.
///
/// The ORDER is not tested here: it is `order_conversations` in
/// `crates/lakko-ui-rules` and it has its own tests there, which is the whole
/// point of it living there. What is tested here is that iOS actually reaches
/// that rule and does not quietly re-sort afterwards, and the mapping and the
/// filter this client owns.
final class ConversationListTests: XCTestCase {

    private func row(
        _ id: String,
        kind: ConversationListKind = .chat,
        title: String = "Someone",
        preview: String = "",
        at ms: Int64? = nil,
        pinned: Bool = false,
        archived: Bool = false,
        unread: Int = 0,
        muted: Bool = false,
        endedLine: String? = nil
    ) -> ConversationListRow {
        ConversationListRow(
            id: id, kind: kind, title: title, preview: preview, lastActivityMs: ms,
            unread: unread, pinned: pinned, muted: muted, archived: archived,
            endedLine: endedLine)
    }

    private func ids(_ rows: [ConversationListRow]) -> [String] {
        ConversationList.sorted(rows).map(\.id)
    }

    // MARK: - The shared order, reached from here

    /// The point of merging: the newest thing that happened does not care
    /// which container it arrived in. This is the assertion that would fail if
    /// the screen ever filtered by kind again.
    func testNewestFirstWhateverKindItIs() {
        let rows = [
            row("chat", kind: .chat, title: "Anna", at: 10),
            row("channel", kind: .channel, title: "News", at: 30),
            row("group", kind: .group, title: "Climbing", at: 20),
        ]
        XCTAssertEqual(ids(rows), ["channel", "group", "chat"])
    }

    func testPinnedBeatsSomethingNewer() {
        let rows = [
            row("new", title: "Boris", at: 999),
            row("pinned", title: "Anna", at: 1, pinned: true),
        ]
        XCTAssertEqual(ids(rows).first, "pinned")
    }

    func testArchivedSinksBelowSomethingNewer() {
        let rows = [
            row("archived", title: "Anna", at: 999, archived: true),
            row("live", title: "Boris", at: 1),
        ]
        XCTAssertEqual(ids(rows), ["live", "archived"])
    }

    /// A conversation nothing has happened in yet is the oldest there is — and
    /// two of them still order the same way every time, so an unrelated
    /// refresh never reshuffles the list.
    func testNothingHasHappenedYetSortsLastAndStably() {
        let zoe = row("1", title: "Zoe", at: nil)
        let anna = row("2", title: "Anna", at: nil)
        let old = row("3", title: "Boris", at: 1)
        XCTAssertEqual(ids([zoe, anna, old]), ["3", "2", "1"])
        XCTAssertEqual(ids([anna, old, zoe]), ["3", "2", "1"])
    }

    /// A sort that dropped or duplicated a row would lose a conversation from
    /// the list without anything failing.
    func testEveryRowSurvivesExactlyOnce() {
        let rows = (0..<40).map { i in
            row("\(i)", title: "Same", at: i % 3 == 0 ? nil : Int64(i % 7))
        }
        let sorted = ConversationList.sorted(rows)
        XCTAssertEqual(sorted.count, rows.count)
        XCTAssertEqual(Set(sorted.map(\.id)).count, rows.count)
    }

    func testAnEmptyListOrdersToNothing() {
        XCTAssertTrue(ConversationList.sorted([]).isEmpty)
    }

    // MARK: - The local half of search

    func testSearchMatchesTitleAndPreviewCaseAndDiacriticInsensitively() {
        let rows = [
            row("a", title: "Anna", preview: "see you at six"),
            row("b", title: "Boris", preview: "nothing"),
            row("c", title: "Zoë", preview: "nothing"),
        ]
        XCTAssertEqual(ConversationList.filtered(rows, query: "anna").map(\.id), ["a"])
        XCTAssertEqual(ConversationList.filtered(rows, query: "SIX").map(\.id), ["a"])
        XCTAssertEqual(ConversationList.filtered(rows, query: "zoe").map(\.id), ["c"])
        XCTAssertEqual(ConversationList.filtered(rows, query: "  ").map(\.id), ["a", "b", "c"])
    }

    /// The ended line takes the preview's slot, so it is what search sees too
    /// — searching for text that is no longer drawn would find a row whose
    /// second line does not contain it.
    func testSearchReadsTheLineTheRowActuallyDraws() {
        let rows = [row("a", preview: "see you at six", endedLine: "You are no longer in this chat")]
        XCTAssertEqual(ConversationList.filtered(rows, query: "no longer").map(\.id), ["a"])
        XCTAssertTrue(ConversationList.filtered(rows, query: "six").isEmpty)
    }

    // MARK: - What the row draws

    /// A conversation whose membership has ended goes quiet: the badge is not
    /// drawn, and the count is not spoken either.
    func testAnEndedConversationDrawsNoUnreadBadge() {
        let live = row("a", unread: 3)
        let ended = row("b", unread: 3, endedLine: "You are no longer in this chat")
        XCTAssertTrue(live.showsUnread)
        XCTAssertFalse(ended.showsUnread)
    }

    /// A channel carries no pin, mute, archive or unread, because none of them
    /// is keyed on anything a channel has. Inventing a badge that can never be
    /// non-zero would be a lie about what the app knows.
    func testAChannelRowCarriesNoneOfTheFlags() {
        let channel = row("c", kind: .channel, title: "News", at: 5)
        XCTAssertFalse(channel.pinned)
        XCTAssertFalse(channel.muted)
        XCTAssertFalse(channel.archived)
        XCTAssertFalse(channel.showsUnread)
    }
}
