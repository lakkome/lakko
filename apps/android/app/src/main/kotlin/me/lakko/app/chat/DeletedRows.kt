// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * `docs/ux/message-actions.md` section 9 — a deleted message leaves the
 * conversation. The wire and the stored record do not change (the ADR-0023
 * delete op, the row kept with `deleted = true`); what changes is only what is
 * drawn:
 *
 *  - a deleted message is not drawn;
 *  - except when its deletion was applied in front of the person (this
 *    conversation open, the lifecycle at least RESUMED, the row already in the
 *    timeline before the deletion): then the bubble reads "Message deleted"
 *    for [DeletedRows.HOLD_MS], collapses over [DeletedRows.COLLAPSE_MS] and is
 *    gone.
 *
 * The hold is kept in memory only, never persisted. Everything here is pure
 * so the host JVM suite covers it; the timers live in the view model and the
 * animation in the screen.
 */

/** Where a held deleted row is in its short life on screen. */
public enum class DeletedRowPhase {
    /** Drawn as "Message deleted". */
    HOLD,

    /** Collapsing to zero height; removed when the collapse ends. */
    COLLAPSE,
}

/** The rules, each a pure function. */
public object DeletedRows {
    /** How long a deleted row reads "Message deleted" (chosen, not measured). */
    public const val HOLD_MS: Long = 2_000L

    /** How long the row takes to collapse (chosen, not measured). */
    public const val COLLAPSE_MS: Long = 250L

    /** The ids of the rows a timeline built from [messages] draws as
     *  messages: every message with a server id that is not deleted. */
    public fun renderedIds(messages: List<ChatMessage>): Set<Long> {
        val out = HashSet<Long>()
        for (m in messages) if (m.messageId != 0L && !m.deleted) out += m.messageId
        return out
    }

    /**
     * The rows that enter the hold: deleted now, drawn before the deletion was
     * applied, and only when the conversation is in front of the person.
     * A message and its deletion in one batch was never drawn, so it is not
     * here; neither is anything applied while the conversation was elsewhere.
     */
    public fun newlyHeld(renderedBefore: Set<Long>, messages: List<ChatMessage>, inFront: Boolean): Set<Long> {
        if (!inFront || renderedBefore.isEmpty()) return emptySet()
        val out = LinkedHashSet<Long>()
        for (m in messages) if (m.deleted && m.messageId != 0L && m.messageId in renderedBefore) out += m.messageId
        return out
    }

    /** [messages] without the deleted ones, except those in [held]. */
    public fun visible(messages: List<ChatMessage>, held: Set<Long>): List<ChatMessage> {
        if (messages.none { it.deleted }) return messages
        return messages.filter { !it.deleted || (it.messageId != 0L && it.messageId in held) }
    }

    /**
     * The row a conversation's list preview is taken from: the newest message
     * or call row that is not deleted. The receive loop's bookkeeping rows (a
     * commit, an external-join request) never are. `null` when none is left:
     * the list then says what it says for a conversation with no messages.
     */
    public fun previewSource(messages: List<ChatMessage>): ChatMessage? =
        messages.lastOrNull { !it.deleted && (it.direction != Direction.System || it.call != null) }
}

/**
 * The in-memory hold of one open conversation. Fed every new message list
 * with whether the conversation is in front; the caller drives the timers
 * ([startCollapse], [release]) and clears the hold when the conversation leaves
 * the front ([leaveFront]). Not thread-safe: the view model drives it from the
 * main thread.
 */
public class DeletedRowHold {
    private var rendered: Set<Long> = emptySet()
    private val held = LinkedHashMap<Long, DeletedRowPhase>()

    /** The rows held now and their phase. */
    public val phases: Map<Long, DeletedRowPhase> get() = LinkedHashMap(held)

    /**
     * A new message list was applied. Returns the ids that entered the hold
     * with it (the caller starts their timers). A held id the list no longer
     * holds leaves the hold.
     */
    public fun observe(messages: List<ChatMessage>, inFront: Boolean): Set<Long> {
        val deleted = HashSet<Long>()
        val present = HashSet<Long>()
        for (m in messages) {
            if (m.messageId == 0L) continue
            present += m.messageId
            if (m.deleted) deleted += m.messageId
        }
        return observeIds(DeletedRows.renderedIds(messages), deleted, present, inFront)
    }

    /**
     * The same, for a timeline that is not a list of [ChatMessage] (a
     * channel's posts): [drawn] the ids it draws now, [deleted] the ids it
     * knows were deleted, [present] every id it still knows (drawn or
     * deleted). A held id no longer [present] leaves the hold.
     */
    public fun observeIds(drawn: Set<Long>, deleted: Set<Long>, present: Set<Long>, inFront: Boolean): Set<Long> {
        val entered = LinkedHashSet<Long>()
        if (inFront) {
            for (id in deleted) if (id in rendered && id !in held) entered += id
        }
        for (id in entered) held[id] = DeletedRowPhase.HOLD
        if (held.isNotEmpty()) held.keys.retainAll(present)
        rendered = drawn
        return entered
    }

    /** The hold of [id] ended: it starts collapsing. `false` when [id] is not
     *  held (it left meanwhile). */
    public fun startCollapse(id: Long): Boolean {
        if (held[id] == null) return false
        held[id] = DeletedRowPhase.COLLAPSE
        return true
    }

    /** The collapse of [id] ended: the row is gone. */
    public fun release(id: Long): Boolean = held.remove(id) != null

    /** The conversation left the front: every held row is gone at once (it is
     *  not there when the person comes back). The drawn set stays, so a row
     *  drawn before the person left still counts as drawn. */
    public fun leaveFront() {
        held.clear()
    }
}
