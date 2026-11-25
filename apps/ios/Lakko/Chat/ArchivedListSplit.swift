// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Where the archived conversations sit in the list
/// (`docs/ux/navigation.md` section 2.2, step 3 and its note "Archived, as
/// built (2026-11-25)").
///
/// **The rule is not here.** It is `archived_section` in
/// `crates/lakko-ui-rules`, shared with web and Android: archived
/// conversations are never in the chats list; one "Archived" bar fixed to
/// the bottom of the panel opens them as a list of their own; while something
/// is typed, the chats view lists the archived matches under a divider
/// instead. It answers in counts over the ordered list; this adapter hands it
/// the two flags of each row and slices the client's rows by the counts.
public enum ArchivedListSplit {

    /// Which of the two lists the panel shows.
    public enum Pane: Equatable {
        /// The conversations that are not archived.
        case chats
        /// The archived conversations, opened from the bar.
        case archive
    }

    /// What the panel draws, split around the archived conversations.
    public struct Split<Row> {
        /// The list shown.
        public let pane: Pane
        /// The conversations that are not archived (with the pins), in order.
        public let top: [Row]
        /// The archived conversations, in order.
        public let archived: [Row]
        /// The count the "Archived" bar carries, or `nil` when no bar stands
        /// (the archive view, a search, nothing archived).
        public let barCount: Int?
        /// Chats view while searching: the archived matches follow the others
        /// under a divider.
        public let divider: Bool
    }

    /// Split an ordered list by the shared rule. `isPinned` is the pin of the
    /// view shown: the folder's own pin inside a folder. `archiveOpen`: the
    /// person opened the archive from the bar and has not gone back.
    public static func split<Row>(
        _ rows: [Row],
        isArchived: (Row) -> Bool,
        isPinned: (Row) -> Bool,
        searching: Bool,
        archiveOpen: Bool
    ) -> Split<Row> {
        let section = LakkoCore.archivedSection(
            rows: rows.map { ArchivedSectionRow(archived: isArchived($0), pinned: isPinned($0)) },
            searching: searching, archiveOpen: archiveOpen)
        let topCount = min(Int(section.topCount), rows.count)
        let end = min(topCount + Int(section.archivedCount), rows.count)
        return Split(
            pane: section.view == .archive ? .archive : .chats,
            top: Array(rows[..<topCount]),
            archived: Array(rows[topCount..<end]),
            barCount: section.bar ? Int(section.archivedCount) : nil,
            divider: section.divider)
    }
}
