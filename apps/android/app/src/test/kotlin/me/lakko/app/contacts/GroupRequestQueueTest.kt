// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.chat.CursorVerdict
import me.lakko.app.chat.DurableCursorGate
import me.lakko.app.chat.applyDurableCursorBatch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/who-can-find-me.md` 6.4 — an opened group contact request is never
 * lost to a crash: it is on disk before the group journal's cursor moves past
 * its row, or the cursor stays put and the row is read again.
 */
class GroupRequestQueueTest {

    private val account = "QUNDVA=="

    private fun entry(id: Long) = StoredGroupRequest(
        messageId = id,
        groupIdHex = "abcd",
        wireB64 = "d2lyZQ==",
        createdAt = "2026-11-05T10:00:00Z",
    )

    @Test
    fun `a stored request survives a restart`() {
        val store = InMemoryContactsInboxStore()
        assertTrue(GroupRequestQueue(store, account).add(entry(-7L)))
        // A new process: a new queue over the same store.
        val afterRestart = GroupRequestQueue(store, account)
        assertEquals(listOf(entry(-7L)), afterRestart.all())
    }

    @Test
    fun `a cursor never passes an unstored request`() {
        val store = InMemoryContactsInboxStore()
        val queue = GroupRequestQueue(store, account)
        store.failGroupWrites = true
        assertFalse(queue.add(entry(-7L)))
        // Still listed in this process.
        assertEquals(listOf(entry(-7L)), queue.all())

        val gate = DurableCursorGate()
        var cursor = 10L
        var snapshotWritten = false
        val held = applyDurableCursorBatch(
            gate = gate,
            touchedMlsState = true,
            persist = {
                queue.flushOrThrow()
                snapshotWritten = true
            },
            advance = { cursor = 11L },
        )
        assertEquals(CursorVerdict.HELD, held)
        assertEquals(10L, cursor)
        // The snapshot that consumed the row is not written either, so after a
        // crash the row is read again with the old state.
        assertFalse(snapshotWritten)
        assertTrue(GroupRequestQueue(store, account).all().isEmpty())

        // The write lands on the next pass: then, and only then, the cursor moves.
        store.failGroupWrites = false
        val advanced = applyDurableCursorBatch(
            gate = gate,
            touchedMlsState = true,
            persist = {
                queue.flushOrThrow()
                snapshotWritten = true
            },
            advance = { cursor = 11L },
        )
        assertEquals(CursorVerdict.ADVANCED, advanced)
        assertEquals(11L, cursor)
        assertTrue(snapshotWritten)
        assertEquals(listOf(entry(-7L)), GroupRequestQueue(store, account).all())
    }

    @Test
    fun `an inbox pass that listed them lets them go`() {
        val store = InMemoryContactsInboxStore()
        val queue = GroupRequestQueue(store, account)
        queue.add(entry(-7L))
        queue.add(entry(-8L))
        queue.remove(setOf(-7L))
        assertEquals(listOf(entry(-8L)), GroupRequestQueue(store, account).all())
    }

    @Test
    fun `the same row opened twice is kept once`() {
        val store = InMemoryContactsInboxStore()
        val queue = GroupRequestQueue(store, account)
        queue.add(entry(-7L))
        queue.add(entry(-7L))
        assertEquals(1, queue.all().size)
    }

    @Test
    fun `nothing held in memory is a flush that never throws`() {
        GroupRequestQueue(InMemoryContactsInboxStore(), account).flushOrThrow()
    }
}
