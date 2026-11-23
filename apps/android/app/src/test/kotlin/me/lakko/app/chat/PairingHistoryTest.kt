// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import me.lakko.app.devices.HistoryBundleCodec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/second-device.md` section 2: what a pairing history bundle leaves
 * on a device. The log is written whole from the chat layer's memory, so these
 * walk the order a new device goes through (bundle on disk, bootstrap holding
 * no group, a Welcome join, the write that follows it) over the production
 * seams; the repository around them needs a live MLS group.
 */
class PairingHistoryTest {

    private val joined = "aa".repeat(32)
    private val notYet = "bb".repeat(32)

    private fun row(id: Long, text: String = "m$id", direction: String = "incoming") =
        PersistedMessage(id, direction, text, emptyList(), "2026-10-30T10:00:00Z")

    /** The slot the repository writes for a record holding [rows]. */
    private fun heldSlot(rows: List<PersistedMessage>, floor: Long? = null) =
        PersistedGroup(cursor = 0L, messages = rows, syncedThroughMessageId = floor)

    @Test
    fun `the write after a join keeps the bundle's rows, for the joined group and for the others`() {
        // The bundle lands while the device holds no group.
        val onDisk = HistoryBundleCodec.mergeBundle(
            emptyMap(),
            mapOf(joined to listOf(row(1), row(2)), notYet to listOf(row(7))),
        ).groups

        // The bootstrap seeds nothing: no group is held yet.
        val waiting = WaitingLogSlots().apply { fillFrom(onDisk, heldGroups = emptySet()) }

        // A Welcome joins one group: the new record takes its slot...
        val taken = waiting.take(joined)
        val slot = heldSlot(taken?.messages.orEmpty(), taken?.syncedThroughMessageId)

        // ...and the join's own persist writes the whole log.
        val written = composeMessageLog(mapOf(joined to slot), waiting.snapshot())

        assertEquals(listOf(1L, 2L), written[joined]?.messages?.map { it.messageId })
        assertEquals(listOf(7L), written[notYet]?.messages?.map { it.messageId })
        assertEquals(2L, written[joined]?.syncedThroughMessageId)
        assertEquals(7L, written[notYet]?.syncedThroughMessageId)
    }

    @Test
    fun `a restart before any join keeps the rows, and the group joined afterwards still takes them`() {
        val onDisk = HistoryBundleCodec.mergeBundle(emptyMap(), mapOf(joined to listOf(row(1)))).groups
        val first = WaitingLogSlots().apply { fillFrom(onDisk, heldGroups = emptySet()) }
        val afterRestart = composeMessageLog(emptyMap(), first.snapshot())

        val second = WaitingLogSlots().apply { fillFrom(afterRestart, heldGroups = emptySet()) }
        assertEquals(listOf(1L), second.take(joined)?.messages?.map { it.messageId })
        assertNull("a slot is taken once", second.take(joined))
    }

    @Test
    fun `a bundle reaching a group already in memory, or waiting, merges by id`() {
        val waiting = WaitingLogSlots()
        waiting.add(notYet, heldSlot(listOf(row(1), row(2)), floor = 2))
        waiting.add(notYet, heldSlot(listOf(row(2, "kept as held"), row(3)), floor = 3))
        val slot = waiting.take(notYet)
        assertEquals(listOf(1L, 2L, 3L), slot?.messages?.map { it.messageId })
        assertEquals("m2", slot?.messages?.get(1)?.text)
        assertEquals(3L, slot?.syncedThroughMessageId)
    }

    @Test
    fun `the record's rows win over a waiting slot of the same group`() {
        val written = composeMessageLog(
            mapOf(joined to heldSlot(listOf(row(5)))),
            mapOf(joined to heldSlot(listOf(row(1)))),
        )
        assertEquals(listOf(5L), written[joined]?.messages?.map { it.messageId })
    }

    @Test
    fun `sign-out empties the waiting slots`() {
        val waiting = WaitingLogSlots()
        waiting.add(joined, heldSlot(listOf(row(1))))
        waiting.clear()
        assertEquals(emptyMap<String, PersistedGroup>(), waiting.snapshot())
    }

    @Test
    fun `the floor is the bundle's highest id, moves up only, and leaves the slot's other fields alone`() {
        val held = mapOf(
            joined to PersistedGroup(
                cursor = 40L,
                messages = listOf(row(39)),
                feedSeen = listOf(38L),
                feedSeenThrough = 30L,
                syncedThroughMessageId = 20L,
            ),
        )
        val higher = HistoryBundleCodec.mergeBundle(held, mapOf(joined to listOf(row(21), row(25))))
        val slot = higher.groups.getValue(joined)
        assertEquals(25L, slot.syncedThroughMessageId)
        assertEquals(40L, slot.cursor)
        assertEquals(listOf(38L), slot.feedSeen)
        assertEquals(30L, slot.feedSeenThrough)
        assertEquals(listOf(21L, 25L, 39L), slot.messages.map { it.messageId })
        assertEquals(2, higher.applied)

        val lower = HistoryBundleCodec.mergeBundle(higher.groups, mapOf(joined to listOf(row(3))))
        assertEquals(25L, lower.groups.getValue(joined).syncedThroughMessageId)
    }

    @Test
    fun `what a running chat layer is handed is the bundle alone, with its floor`() {
        val held = mapOf(joined to heldSlot(listOf(row(39))))
        val opened = HistoryBundleCodec.mergeBundle(held, mapOf(joined to listOf(row(7), row(2)))).opened
        assertEquals(listOf(2L, 7L), opened.getValue(joined).messages.map { it.messageId })
        assertEquals(7L, opened.getValue(joined).syncedThroughMessageId)
    }

    @Test
    fun `a bundle of rows with no server id sets no floor`() {
        assertNull(bundleFloor(listOf(row(-1), row(-2))))
        assertNull(bundleFloor(emptyList()))
        assertEquals(9L, bundleFloor(listOf(row(-1), row(9), row(4))))
    }

    @Test
    fun `the floor survives a write and a read of the log, and a log from before it reads as none`() {
        val payload = MessageLogPayload(groups = mapOf(joined to heldSlot(listOf(row(1)), floor = 1)))
        val json = Json.encodeToString(MessageLogPayload.serializer(), payload)
        assertEquals(1L, MessageLogStore.decodeLogPayload(json)?.groups?.get(joined)?.syncedThroughMessageId)

        val older = """{"version":1,"groups":{"$joined":{"cursor":3,"messages":[]}}}"""
        assertNull(MessageLogStore.decodeLogPayload(older)?.groups?.get(joined)?.syncedThroughMessageId)
    }

    // ----- Rows in memory -------------------------------------------------

    private fun chat(id: Long, ack: Ack = Ack.SENT) =
        ChatMessage(id, Direction.Incoming, "m$id", createdAt = "t", ack = ack)

    @Test
    fun `bundle rows go in id order around what the record already holds`() {
        val held = listOf(chat(10), chat(20))
        val merged = mergeHistoryRows(held, listOf(chat(5), chat(15), chat(25)))
        assertEquals(listOf(5L, 10L, 15L, 20L, 25L), merged.map { it.messageId })
    }

    @Test
    fun `a row held is kept as held and a local row of the bundle with no time goes first`() {
        val held = listOf(chat(10).copy(text = "polled"))
        val merged = mergeHistoryRows(held, listOf(chat(-1), chat(10)))
        assertEquals(listOf(-1L, 10L), merged.map { it.messageId })
        assertEquals("polled", merged.last().text)
    }

    private val callAt0901 = Long.MIN_VALUE or 0x0102_0304_0506_0708L

    @Test
    fun `a call row of the bundle sits at its time among the messages`() {
        val held = listOf(
            chat(1).copy(createdAt = "2026-11-23T09:00:00Z"),
            chat(2).copy(createdAt = "2026-11-23T09:02:00Z"),
        )
        val call = ChatMessage(callAt0901, Direction.System, "Declined", createdAt = "2026-11-23T09:01:00Z")
        val merged = mergeHistoryRows(held, listOf(call))
        assertEquals(listOf(1L, callAt0901, 2L), merged.map { it.messageId })
        // Applied again: one row.
        assertEquals(merged, mergeHistoryRows(merged, listOf(call)))
    }

    @Test
    fun `the log on disk places a call row at its time too`() {
        val held = listOf(
            row(1).copy(createdAt = "2026-11-23T09:00:00Z"),
            row(2).copy(createdAt = "2026-11-23T09:02:00Z"),
        )
        val call = PersistedMessage(callAt0901, "system", "", emptyList(), "2026-11-23T09:01:00Z")
            .copy(call = PersistedCall(outcome = "declined"))
        val (merged, added) = HistoryBundleCodec.mergeRows(held, listOf(call, row(3).copy(createdAt = "2026-11-23T09:03:00Z")))
        assertEquals(listOf(1L, callAt0901, 2L, 3L), merged.map { it.messageId })
        assertEquals(2, added)
        val (again, none) = HistoryBundleCodec.mergeRows(merged, listOf(call))
        assertEquals(merged, again)
        assertEquals(0, none)
    }

    @Test
    fun `a call row never lands after a row still being sent`() {
        val sending = chat(0, ack = Ack.PENDING).copy(createdAt = "2026-11-23T09:05:00Z")
        val held = listOf(chat(1).copy(createdAt = "2026-11-23T09:00:00Z"), sending)
        val call = ChatMessage(callAt0901, Direction.System, "Declined", createdAt = "2026-11-23T09:10:00Z")
        assertEquals(listOf(1L, callAt0901, 0L), mergeHistoryRows(held, listOf(call)).map { it.messageId })
    }

    @Test
    fun `a bundle row never lands after a row still being sent`() {
        val sending = chat(0, ack = Ack.PENDING)
        val merged = mergeHistoryRows(listOf(chat(10), sending), listOf(chat(12)))
        assertEquals(listOf(10L, 12L, 0L), merged.map { it.messageId })
    }

    @Test
    fun `a polled row the bundle already brought is not added twice, a later one is`() {
        val rows = listOf(chat(1), chat(2))
        assertTrue(heldFromBundle(floor = 2, messageId = 2, rows = rows))
        assertFalse("not held: it joins the timeline", heldFromBundle(floor = 5, messageId = 3, rows = rows))
        assertFalse("past the floor", heldFromBundle(floor = 1, messageId = 2, rows = rows))
        assertFalse("no bundle", heldFromBundle(floor = null, messageId = 2, rows = rows))
    }
}
