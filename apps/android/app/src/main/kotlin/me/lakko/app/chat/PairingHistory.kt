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
 * before a row the server has not named yet), and a row the sender made
 * locally (a call row, its id not the server's) goes first, as the log's
 * order by id keeps it.
 */
internal fun mergeHistoryRows(held: List<ChatMessage>, bundle: List<ChatMessage>): List<ChatMessage> {
    val ids = held.mapTo(HashSet()) { it.messageId }
    val fresh = bundle.filter { ids.add(it.messageId) }
    val (journal, local) = fresh.partition { it.messageId > 0 }
    val out = ArrayList<ChatMessage>(held.size + fresh.size)
    out += local
    val base = out.size
    out += held
    var from = base
    for (row in journal.sortedBy { it.messageId }) {
        var at = from
        while (at < out.size && out[at].messageId <= row.messageId && out[at].ack == Ack.SENT) at += 1
        out.add(at, row)
        from = at + 1
    }
    return out
}

/** A polled message that is already held from a bundle: the same row came
 *  twice (sent after the Welcome, before the bundle was made). */
internal fun heldFromBundle(floor: Long?, messageId: Long, rows: List<ChatMessage>): Boolean =
    floor != null && messageId <= floor && rows.any { it.messageId == messageId }
