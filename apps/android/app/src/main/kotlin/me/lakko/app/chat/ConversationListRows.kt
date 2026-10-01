// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.ConversationKind as SharedConversationKind
import uniffi.lakko_ffi_uniffi.ConversationRow as SharedConversationRow

/**
 * One list for every conversation: one-to-one chats, groups and channels.
 *
 * `docs/ux/navigation.md` section 2. Chats, Groups and Channels used to be
 * three tabs for one activity — a person does not think "I will go to the
 * Groups place", they think of a conversation and look for it — and a list
 * sorted by WHEN cannot be three lists.
 *
 * **The order itself is not here.** It is `order_conversations` in
 * `crates/lakko-ui-rules`, shared with web and iOS; what this file owns is the
 * mapping between the rich row Compose draws and the six fields that order
 * reads. Chats and groups were already one store split by a filter, so they
 * merge by dropping it; channels are their own store and have to be mapped in,
 * which is the real work.
 *
 * **The row says nothing about which kind it is.** [ConversationListRow.kind]
 * is here to route a tap and for nothing else: no badge, no label, no
 * differing avatar. A person who subscribed to a channel knows they subscribed
 * to a channel.
 *
 * Pure of Compose and of Android, so the mapping and the reordering can be run
 * by a host JVM unit test: the shared rule arrives as a function parameter
 * whose default is the real binding, the way [me.lakko.app.profile.checkUsername]
 * takes the core's username rule.
 */

/** Which screen a row opens. Never rendered — see the file docstring. */
public typealias ConversationRowKind = SharedConversationKind

/** One row of the merged list, as Compose draws it. */
public data class ConversationListRow(
    /** Stable within a kind: the group id hex for a chat or a group, the
     *  channel id for a channel. The pair `(kind, id)` is unique in the list. */
    val id: String,
    val kind: ConversationRowKind,
    val title: String,
    /** The last message or post, already flattened to one line. */
    val preview: String,
    /** The formatted last-activity time, or `null` when there is none to show. */
    val time: String?,
    /**
     * When the last thing a person can SEE happened, in unix millis, or `null`
     * for a conversation nothing has happened in yet. A row that draws nothing
     * does not move a conversation to the top.
     */
    val lastActivityMs: Long?,
    /**
     * Unread, pinned, muted and archived are keyed on a conversation id a
     * channel does not carry (`docs/ux/navigation.md` section 6.1), so a
     * channel row reports none of them. That is honest; inventing a badge that
     * can never be non-zero is not.
     */
    val unread: Int = 0,
    val pinned: Boolean = false,
    val muted: Boolean = false,
    val archived: Boolean = false,
    /** A manual "mark as unread" (the synced flag). With [unread] it is what
     *  a folder's Read toggle reads (`docs/ux/chat-folders.md` 3.2). */
    val unreadOverride: Boolean = false,
    /** This device's membership has ended (left, or removed). Still listed in
     *  All chats with its ended line; a folder matches nothing it cannot
     *  open (`docs/ux/chat-folders.md` 3.2). */
    val membershipEnded: Boolean = false,
)

/** The shared kind a chat-or-group row opens under. */
public fun ConversationKind.toRowKind(): ConversationRowKind = when (this) {
    ConversationKind.Chat -> SharedConversationKind.CHAT
    ConversationKind.Group -> SharedConversationKind.GROUP
}

/** The six fields the shared order reads, and nothing else. */
internal fun ConversationListRow.toSharedRow(): SharedConversationRow = SharedConversationRow(
    id = id,
    kind = kind,
    title = title,
    lastActivityMs = lastActivityMs,
    pinned = pinned,
    archived = archived,
)

/**
 * Pinned first, archived last, newest between — decided by the shared rule.
 *
 * The rule hands back the input INDICES rather than rows, so the avatar, the
 * preview and the unread count never cross the FFI boundary at all: this
 * reorders the list it was already given.
 *
 * An index the rule could not name (it cannot happen, but a `UInt` from across
 * a boundary is still an input) drops its row rather than throwing: losing one
 * row from a list is recoverable, and crashing the only screen the app opens on
 * is not.
 */
public fun orderConversationRows(
    rows: List<ConversationListRow>,
    order: (List<SharedConversationRow>) -> List<UInt> = {
        uniffi.lakko_ffi_uniffi.orderConversations(it)
    },
): List<ConversationListRow> {
    if (rows.isEmpty()) return emptyList()
    return order(rows.map { it.toSharedRow() }).mapNotNull { index ->
        rows.getOrNull(index.toInt())
    }
}

/**
 * Whether a row matches what was typed.
 *
 * Local and loose, over titles and previews this device already holds — which
 * is the half of `docs/ux/navigation.md` section 4 that may be fuzzy at all.
 * The network lookup next to it can only ever be exact, and lives in
 * [findByName].
 */
public fun conversationRowMatchesQuery(
    query: String,
    title: String,
    preview: String,
): Boolean {
    val q = query.trim()
    if (q.isEmpty()) return true
    return title.contains(q, ignoreCase = true) || preview.contains(q, ignoreCase = true)
}

/**
 * Whether the conversation list shows its empty state (the sentence and the
 * New button) rather than what a search finds.
 *
 * `docs/ux/navigation.md` section 4, "Also with an empty list": the search
 * field is there before the first conversation exists, because finding the
 * first person to talk to is what a new account needs it for. With nothing
 * typed the empty state shows; once something is typed it gives way to the
 * search, the people half included (`docs/ux/who-can-find-me.md` 6.1).
 */
public fun showsEmptyConversationState(listIsEmpty: Boolean, query: String): Boolean =
    listIsEmpty && query.isBlank()
