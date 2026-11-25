// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.ArchivedSection as SharedArchivedSection
import uniffi.lakko_ffi_uniffi.ArchivedSectionRow as SharedArchivedSectionRow
import uniffi.lakko_ffi_uniffi.ArchivedView

/**
 * The shared rule, as a function the list calls: `archived_section` in
 * `crates/lakko-ui-rules` over UniFFI. A parameter so the host JVM tests,
 * which cannot load the native library, can hand in a fake and test the
 * wiring; the rule itself is tested in Rust and through the FFI on iOS and
 * web.
 */
public typealias ArchivedSectionRule =
    (rows: List<SharedArchivedSectionRow>, searching: Boolean, archiveOpen: Boolean) -> SharedArchivedSection

/** The real rule over the native library. */
public val realArchivedSectionRule: ArchivedSectionRule = { rows, searching, archiveOpen ->
    uniffi.lakko_ffi_uniffi.archivedSection(rows, searching, archiveOpen)
}

/**
 * Where the archived conversations sit in the list
 * (`docs/ux/navigation.md` section 2.2, "Archived, as built (2026-11-25)").
 *
 * **The rule is not here** (see [ArchivedSectionRule]): archived
 * conversations are never in the chats list; one "Archived" bar at the bottom
 * of the panel opens them as a list of their own; while searching in the
 * chats view, archived matches stand under a divider. It answers in counts
 * over the ordered list; this adapter hands it the two flags of each row and
 * slices the client's rows by the counts.
 */
public object ArchivedListSplit {

    /** What the panel draws. */
    public data class Split<T>(
        /** The list shown: the chats or the archive. */
        val view: ArchivedView,
        /** The rows that are not archived (the pins included), in order. */
        val top: List<T>,
        /** The archived rows, in order. */
        val archived: List<T>,
        /** The count the bottom bar shows, or `null` when there is no bar. */
        val barCount: Int?,
        /** Whether archived search matches stand under a divider. */
        val divider: Boolean,
    ) {
        /** The archive is shown in place of the chats list. */
        val archiveShown: Boolean get() = view == ArchivedView.ARCHIVE

        /** The rows above the divider in the chats view; none in the archive. */
        val chatRows: List<T> get() = if (archiveShown) emptyList() else top

        /**
         * The archived rows drawn in the scrolling list: the whole archive,
         * the matches under the divider, or none behind the bar.
         */
        val archivedRows: List<T> get() = if (archiveShown || divider) archived else emptyList()
    }

    /**
     * Split an ordered list by the shared rule. [isPinned] is the pin of the
     * view shown: the folder's own pin inside a folder.
     */
    public fun <T> split(
        rows: List<T>,
        searching: Boolean,
        archiveOpen: Boolean,
        isArchived: (T) -> Boolean,
        isPinned: (T) -> Boolean,
        rule: ArchivedSectionRule = realArchivedSectionRule,
    ): Split<T> {
        val section = rule(
            rows.map { SharedArchivedSectionRow(archived = isArchived(it), pinned = isPinned(it)) },
            searching,
            archiveOpen,
        )
        val topCount = section.topCount.toInt().coerceIn(0, rows.size)
        val end = (topCount + section.archivedCount.toInt()).coerceIn(topCount, rows.size)
        val archived = rows.subList(topCount, end).toList()
        return Split(
            view = section.view,
            top = rows.subList(0, topCount).toList(),
            archived = archived,
            barCount = if (section.bar && archived.isNotEmpty()) archived.size else null,
            divider = section.divider && archived.isNotEmpty(),
        )
    }
}
