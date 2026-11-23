// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.nextLocalRowId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A call row's id is its call's (`docs/ux/calls.md` section 6), the scheme the
 * web client uses: `0x8000_0000_0000_0000 | first 8 bytes of the call id`.
 */
class CallRowIdTest {
    private val callIdHex = "0102030405060708" + "a1a2a3a4a5a6a7a8"

    @Test
    fun `the id is the top bit and the first eight bytes of the call id`() {
        val id = callRowMessageId(callIdHex)
        assertEquals(Long.MIN_VALUE or 0x0102_0304_0506_0708L, id)
        assertTrue(id!! < 0)
        // Case does not matter, the rest of the call id does not either.
        assertEquals(id, callRowMessageId(callIdHex.uppercase()))
        assertEquals(id, callRowMessageId("0102030405060708" + "ffffffffffffffff"))
        // A call id whose first bytes are all zero is still negative.
        assertEquals(Long.MIN_VALUE, callRowMessageId("00".repeat(16)))
        // The same call id with the top bit already set.
        assertEquals(-1L, callRowMessageId("ff".repeat(16)))
    }

    @Test
    fun `a missing or malformed call id gives no id`() {
        assertNull(callRowMessageId(null))
        assertNull(callRowMessageId("0102"))
        assertNull(callRowMessageId("zz02030405060708"))
    }

    @Test
    fun `the fallback is stable, negative and never a small counter`() {
        val a = fallbackCallRowMessageId("AB".repeat(32), "1700000000000")
        assertEquals(a, fallbackCallRowMessageId("ab".repeat(32), "1700000000000"))
        assertNotEquals(a, fallbackCallRowMessageId("ab".repeat(32), "1700000000001"))
        assertTrue(a < 0)
        assertTrue(a < LOCAL_COUNTER_FLOOR)
    }

    @Test
    fun `the local counter ignores call rows and never wraps`() {
        val rows = listOf(
            ChatMessage(Long.MIN_VALUE, Direction.System, "Declined", createdAt = "t"),
            ChatMessage(-3, Direction.System, "x", createdAt = "t"),
            ChatMessage(10, Direction.Incoming, "hi", createdAt = "t"),
        )
        assertEquals(-4L, nextLocalRowId(rows))
        assertEquals(-1L, nextLocalRowId(listOf(rows[0], rows[2])))
    }

    @Test
    fun `filed rows are keyed by conversation and id and survive a write and a read`() {
        val a = callRowFiledKey("AB".repeat(32), -1)
        val b = callRowFiledKey("cd".repeat(32), -1)
        assertNotEquals(a, b)
        assertEquals("ab".repeat(32) + ":-1", a)
        val stored = encodeCallRowsFiled(linkedSetOf(a, b))
        assertEquals(linkedSetOf(a, b), decodeCallRowsFiled(stored))
        assertTrue(decodeCallRowsFiled(null).isEmpty())
        val many = (0 until CALL_ROWS_FILED_MAX + 5).map { callRowFiledKey("ab", it.toLong()) }
        val kept = decodeCallRowsFiled(encodeCallRowsFiled(many))
        assertEquals(CALL_ROWS_FILED_MAX, kept.size)
        assertEquals(many.last(), kept.last())
    }
}
