// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * What a device does with the history bundle it received at pairing
 * (`docs/ux/second-device.md` section 2).
 *
 * A bundle lands in the message log, for groups this device mostly does not
 * hold yet: the Welcomes that let it into them come later, one by one. Two
 * rules follow.
 *
 *  - The log is written whole from the chat layer's memory, so a group not in
 *    memory keeps its slot here ([WaitingLogSlots]), and a group that is
 *    joined takes its slot into memory the moment it is registered. Without
 *    this the first write after the first join dropped every bundle row.
 *  - History is not news: each group's floor is the highest id its bundle
 *    carried, and a row at or below it is never unread on this device. The
 *    floor is local and moves up only.
 *
 * Pure: the repository holds the state and the locks.
 */

/** The floor a bundle's [rows] set: their highest message id, `null` when
 *  they hold no id the server assigned. */
internal fun bundleFloor(rows: List<PersistedMessage>): Long? =
    rows.maxOfOrNull { it.messageId }?.takeIf { it > 0 }

/** The higher of two floors. A floor never moves down. */
internal fun higherFloor(a: Long?, b: Long?): Long? = listOfNotNull(a, b).maxOrNull()

/** The slots of the log for groups the chat layer does not hold. Not
 *  thread-safe: the repository guards it with the monitor of its groups. */
internal class WaitingLogSlots {
    private val slots = LinkedHashMap<String, PersistedGroup>()

    /** The bootstrap's split of the log: whatever [log] holds for a group
     *  outside [heldGroups] waits here. */
    fun fillFrom(log: Map<String, PersistedGroup>, heldGroups: Set<String>) {
        slots.clear()
        for ((hex, slot) in log) {
            if (hex !in heldGroups) slots[hex] = slot
        }
    }

    /** A bundle reached a group not held: its rows join what already waits. */
    fun add(groupIdHex: String, incoming: PersistedGroup) {
        val slot = slots[groupIdHex]
        slots[groupIdHex] = if (slot == null) {
            incoming
        } else {
            slot.copy(
                messages = me.lakko.app.devices.HistoryBundleCodec.mergeRows(slot.messages, incoming.messages).first,
                syncedThroughMessageId = higherFloor(slot.syncedThroughMessageId, incoming.syncedThroughMessageId),
            )
        }
    }

    /** The slot of [groupIdHex], taken out: its group has just been joined. */
    fun take(groupIdHex: String): PersistedGroup? = slots.remove(groupIdHex)

    fun clear() {
        slots.clear()
    }

    fun snapshot(): Map<String, PersistedGroup> = slots.toMap()
}

/** The whole log a write puts on disk: the [held] groups, and the slots of the
 *  groups that wait for a join. */
internal fun composeMessageLog(
    held: Map<String, PersistedGroup>,
    waiting: Map<String, PersistedGroup>,
): Map<String, PersistedGroup> = waiting + held

/**
 * [held] with a bundle's [bundle] rows taken in: a row already held stays as
 * held, each new row goes before the first held row with a higher id (and
 * before a row the server has not named yet). A row the sender made locally
 * (a call row: its id is not the server's) goes at its time among the rows,
 * so a call at 09:01 sits between messages of 09:00 and 09:02; a local row
 * whose time cannot be read goes first, as before.
 */
internal fun mergeHistoryRows(held: List<ChatMessage>, bundle: List<ChatMessage>): List<ChatMessage> {
    val ids = held.mapTo(HashSet()) { it.messageId }
    val fresh = bundle.filter { ids.add(it.messageId) }
    return placeBundleRows(
        held = held,
        fresh = fresh,
        idOf = { it.messageId },
        timeOf = { me.lakko.app.ui.chat.parseMessageTimeMs(it.createdAt) },
        settled = { it.ack == Ack.SENT },
    )
}

/**
 * The one placement rule for rows a history bundle brings ([fresh], none of
 * them already in [held]); [held] keeps its order.
 *
 *  - A server row (id above zero) goes, in id order, before the first held
 *    row with a higher id or one not [settled] (still being sent).
 *  - A local row (id zero or below: a call row) with a time goes, in time
 *    order, before the first row with a later time or one not settled.
 *  - A local row with no time goes first.
 */
internal fun <T> placeBundleRows(
    held: List<T>,
    fresh: List<T>,
    idOf: (T) -> Long,
    timeOf: (T) -> Long?,
    settled: (T) -> Boolean,
): List<T> {
    val (journal, local) = fresh.partition { idOf(it) > 0 }
    val (timed, timeless) = local.partition { timeOf(it) != null }
    val out = ArrayList<T>(held.size + fresh.size)
    out += timeless
    val base = out.size
    out += held
    var from = base
    for (row in journal.sortedBy(idOf)) {
        var at = from
        while (at < out.size && idOf(out[at]) <= idOf(row) && settled(out[at])) at += 1
        out.add(at, row)
        from = at + 1
    }
    for (row in timed.sortedBy { timeOf(it) }) {
        val t = timeOf(row) ?: continue
        var at = base
        while (at < out.size && settled(out[at]) && (timeOf(out[at])?.let { it <= t } ?: true)) at += 1
        out.add(at, row)
    }
    return out
}

/** A polled message that is already held from a bundle: the same row came
 *  twice (sent after the Welcome, before the bundle was made). */
internal fun heldFromBundle(floor: Long?, messageId: Long, rows: List<ChatMessage>): Boolean =
    floor != null && messageId <= floor && rows.any { it.messageId == messageId }
