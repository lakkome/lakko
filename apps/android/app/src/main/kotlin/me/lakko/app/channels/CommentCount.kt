// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * `docs/ux/group-and-channel-privacy.md` section 13.2 — what follows the first
 * two comments of a collapsed thread, decided by the shared rule
 * `lakko_ui_rules::comments` (web and iOS ask the same one).
 *
 * The service's comment count (ADR-0087 phase B) counts every stored comment,
 * including those this reader hides (a banned author's, one that does not
 * open; section 13.3: "Their comments disappear for everyone and new ones are
 * not shown"), so it never stands alone: nothing is said before the first
 * page is read, the rows read are judged by themselves, and only the rows
 * not read yet are taken from the service.
 */
public sealed interface CollapsedMore {
    /** Nothing (yet): the first page is not read, or every comment shows. */
    public data object None : CollapsedMore

    /** `Show {n} more comments`. */
    public data class Count(val n: Int) : CollapsedMore

    /** `Show more comments`: rows remain and the service gave no count. */
    public data object Unknown : CollapsedMore

    /** The quiet `Comment`: there are none. */
    public data object Comment : CollapsedMore
}

/** The rule, the shared one in `crates/lakko-ui-rules` (`comments`), so the
 *  three clients cannot disagree. [visible] is the comments this reader
 *  shows; [rowsRead] every row the service returned so far. */
public fun collapsedMore(
    visible: Int,
    rowsRead: Int,
    firstRead: Boolean,
    more: Boolean,
    serverCount: Long?,
): CollapsedMore = collapsedMoreOf(
    uniffi.lakko_ffi_uniffi.collapsedComments(
        visible.coerceAtLeast(0).toUInt(),
        rowsRead.coerceAtLeast(0).toUInt(),
        firstRead,
        more,
        serverCount?.coerceAtLeast(0L)?.toULong(),
    ),
)

/** The shared rule's answer as this client's type. Pure. */
public fun collapsedMoreOf(answer: uniffi.lakko_ffi_uniffi.CollapsedComments): CollapsedMore = when (answer) {
    is uniffi.lakko_ffi_uniffi.CollapsedComments.Nothing -> CollapsedMore.None
    is uniffi.lakko_ffi_uniffi.CollapsedComments.Count -> CollapsedMore.Count(answer.n.toInt())
    is uniffi.lakko_ffi_uniffi.CollapsedComments.Unknown -> CollapsedMore.Unknown
    is uniffi.lakko_ffi_uniffi.CollapsedComments.Comment -> CollapsedMore.Comment
}
