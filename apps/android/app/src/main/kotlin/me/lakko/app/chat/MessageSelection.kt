// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.l10n.UiText

/*
 * `docs/ux/message-actions.md` section 8 — Select: Delete and Forward in bulk.
 *
 * A selection is a UI mode and nothing more: it adds no wire surface, and
 * what it does at the end is the one-message Delete or Forward repeated over
 * a set, oldest first, stopping at the first failure. Every rule of the mode
 * lives here — which rows can be selected, the 100 cap, what the count and
 * the helper lines say, the confirmation wording, the progress copy and the
 * in-order runner — so the Compose wiring renders decisions taken once and
 * the host JVM suite can assert each of them.
 *
 * Nothing here touches Compose, the repository, the network or the native
 * core.
 */

/** Section 8: "At most 100 messages can be selected." */
public const val MESSAGE_SELECTION_LIMIT: Int = 100

/** The action card's row that enters the mode. */
public val SELECT_ACTION_LABEL: UiText = UiText("chat.message.select")

/** The selection bar's leading control. */
public val SELECTION_CANCEL_LABEL: UiText = UiText("common.cancel")

/** The helper line under the action bar when Delete is held back. */
public val ONLY_OWN_DELETABLE_HELPER: UiText = UiText("chat.select.delete_helper")

/** What the selection mark states for accessibility purposes. */
public const val SELECTED_STATE_KEY: String = "chat.select.state_selected"

/** What the selection mark states for accessibility purposes. */
public const val NOT_SELECTED_STATE_KEY: String = "chat.select.state_not_selected"

/** The two actions the mode ends in. */
public enum class BulkAction { DELETE, FORWARD }

/**
 * The set of selected messages, split by whose they are, because that split is
 * what the Delete gate reads (section 8: "enabled when the selection is not
 * empty and every selected message is the person's own").
 *
 * Immutable: every change returns a new value, so a screen can hold it in a
 * state flow and compare it. The order the actions run in is NOT kept here —
 * "oldest first" is the timeline's order, read back through
 * [selectedInTimelineOrder] when the action fires.
 */
public data class MessageSelection(
    /** Ids of selected messages this device sent. */
    val own: Set<Long> = emptySet(),
    /** Ids of selected messages somebody else sent. */
    val others: Set<Long> = emptySet(),
) {
    /** Every selected id, whoever sent it. */
    val ids: Set<Long> get() = own + others

    public val count: Int get() = own.size + others.size

    public val isEmpty: Boolean get() = count == 0

    /** Section 8: at the limit further taps do nothing. */
    public val atLimit: Boolean get() = count >= MESSAGE_SELECTION_LIMIT

    public operator fun contains(messageId: Long): Boolean =
        messageId in own || messageId in others

    /**
     * A tap or a long press on a selectable row: a selected row is
     * deselected, an unselected one is selected unless the selection is at
     * the limit, in which case nothing changes. [isOwn] is the row's own
     * gate, decided by the caller the way the single Delete decides it.
     */
    public fun toggle(messageId: Long, isOwn: Boolean): MessageSelection = when {
        messageId in own -> copy(own = own - messageId)
        messageId in others -> copy(others = others - messageId)
        atLimit -> this
        isOwn -> copy(own = own + messageId)
        else -> copy(others = others + messageId)
    }

    /**
     * After a partial failure: keep only the messages that were not acted
     * on, so the person can retry exactly those (section 8: "keeps the
     * unsent messages selected").
     */
    public fun retain(messageIds: Collection<Long>): MessageSelection {
        val keep = messageIds.toSet()
        return copy(own = own.intersect(keep), others = others.intersect(keep))
    }

    /** "N selected", or "100 selected, the limit" at the cap. */
    public val countLabel: UiText
        get() = UiText(
            if (atLimit) "chat.select.count_limit" else "chat.select.count",
            counts = mapOf("count" to count.toULong()),
        )

    /** Section 8's Delete gate: non-empty and every message the person's own. */
    public val deleteEnabled: Boolean get() = !isEmpty && others.isEmpty()

    /**
     * The helper line under the action bar, or `null` for none. It appears
     * exactly when Delete is held back by someone else's message; an empty
     * selection needs no explanation for two disabled buttons.
     */
    public val deleteHelper: UiText?
        get() = if (others.isNotEmpty()) ONLY_OWN_DELETABLE_HELPER else null

    /** Section 8's Forward gate: non-empty. */
    public val forwardEnabled: Boolean get() = !isEmpty

    public companion object {
        /** Entering the mode with the message the card was opened on. */
        public fun starting(messageId: Long, isOwn: Boolean): MessageSelection =
            MessageSelection().toggle(messageId, isOwn)
    }
}

/**
 * Whether a timeline row can be selected at all (section 8: "Rows that
 * cannot be acted on are not selectable and show no mark").
 *
 * The gates are the ones the single actions already compute: not a system
 * row (a date separator is not a message row in the first place), not
 * deleted, and a completed send — the Forward gate of section 5.1, which also
 * rules out a row with no DS id, since an unsent row has none.
 */
public fun canSelectMessage(
    direction: Direction,
    deleted: Boolean,
    ack: Ack,
    messageId: Long,
): Boolean =
    direction != Direction.System && !deleted && ack == Ack.SENT && messageId != 0L

/** [canSelectMessage] read off a row. */
public fun canSelectMessage(message: ChatMessage): Boolean =
    canSelectMessage(
        direction = message.direction,
        deleted = message.deleted,
        ack = message.ack,
        messageId = message.messageId,
    )

/**
 * The selected messages in the order the actions run — the timeline's own
 * order, oldest first. [messages] is the conversation as the screen holds
 * it, already in journal order; a selected id the timeline no longer shows
 * (a row deleted from another device meanwhile) is simply not in the result.
 */
public fun selectedInTimelineOrder(
    selection: MessageSelection,
    messages: List<ChatMessage>,
): List<ChatMessage> =
    messages.filter { it.messageId != 0L && it.messageId in selection }

/** "Delete 12 messages?" / "Delete 1 message?" */
public fun confirmDeleteLabel(count: Int): UiText =
    UiText("chat.select.delete_title", counts = mapOf("count" to count.toULong()))

/** "Deleted 5 of 12." / "Forwarded 5 of 12." — said after a failure. */
public fun progressLabel(kind: BulkAction, done: Int, total: Int): UiText = UiText(
    when (kind) {
        BulkAction.DELETE -> "chat.select.deleted_of"
        BulkAction.FORWARD -> "chat.select.forwarded_of"
    },
    counts = mapOf("done" to done.toULong(), "total" to total.toULong()),
)

/**
 * What the selection mark says for accessibility purposes: named by the
 * message's text, or its kind when it has none, and stating whether it is
 * selected.
 */
public fun selectionMarkLabel(messageLabel: String, selected: Boolean): UiText =
    UiText(
        "common.list_pair",
        texts = mapOf("first" to messageLabel),
        parts = mapOf("second" to UiText(if (selected) SELECTED_STATE_KEY else NOT_SELECTED_STATE_KEY)),
    )

/**
 * The outcome of running an action over a set in order.
 *
 * [done] is how many landed before the run stopped, [total] how many were
 * asked for, [unsent] the ones that were not acted on — the failed one first,
 * then everything after it. [failure] is `null` on full success.
 */
public data class BulkResult<T>(
    val done: Int,
    val total: Int,
    val unsent: List<T>,
    val failure: Throwable?,
) {
    public val complete: Boolean get() = failure == null && done == total
}

/**
 * Run [action] over [items] in the order given, stopping at the first
 * failure (section 8: "On a failure it stops, keeps the unsent messages
 * selected").
 *
 * A cancellation is not a failure and is rethrown: the caller's scope is
 * going away and there is nobody left to show a count to.
 */
public suspend fun <T> performInOrder(
    items: List<T>,
    action: suspend (T) -> Unit,
): BulkResult<T> {
    for ((index, item) in items.withIndex()) {
        try {
            action(item)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            return BulkResult(
                done = index,
                total = items.size,
                unsent = items.subList(index, items.size),
                failure = e,
            )
        }
    }
    return BulkResult(done = items.size, total = items.size, unsent = emptyList(), failure = null)
}
