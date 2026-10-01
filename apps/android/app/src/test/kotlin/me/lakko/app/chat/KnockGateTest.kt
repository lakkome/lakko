// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ds.MailboxMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.Role

/**
 * ADR-0053 section 4 — knock rows are an unauthenticated write, so every one
 * goes to the knock gate (`processKnock`) and nowhere else; a refused row is
 * dropped and the cursor still moves past it.
 */
class KnockGateTest {

    private fun row(id: Long, tag: Int) = MailboxMessage(id = id, payload = byteArrayOf(tag.toByte()), createdAt = "t$id")

    /** A fake group with both entry points: only the knock gate may be used. */
    private class FakeGroup(private val admit: Set<Int>) {
        val knockGateCalls = mutableListOf<Int>()
        val ordinaryApplyCalls = mutableListOf<Int>()

        fun processKnock(bytes: ByteArray): String {
            val tag = bytes[0].toInt()
            knockGateCalls += tag
            if (tag !in admit) throw IllegalStateException("not an external-join proposal")
            return "join-$tag"
        }

        @Suppress("unused")
        fun processIncomingVerifyingRoles(bytes: ByteArray): String {
            ordinaryApplyCalls += bytes[0].toInt()
            return "applied"
        }
    }

    @Test
    fun `every knock row goes to the knock gate and none to the ordinary apply`() {
        val group = FakeGroup(admit = setOf(1))
        val drained = drainKnockRows(listOf(row(10, 1), row(11, 2), row(12, 3)), after = 9) { bytes ->
            group.processKnock(bytes)
        }
        assertEquals(listOf(1, 2, 3), group.knockGateCalls)
        assertTrue("a knock row must never reach the ordinary receive path", group.ordinaryApplyCalls.isEmpty())
        assertEquals(listOf(10L), drained.accepted.map { it.first.id })
        assertEquals(listOf("join-1"), drained.accepted.map { it.second })
    }

    @Test
    fun `a refused row is dropped and the cursor still advances past it`() {
        val group = FakeGroup(admit = emptySet())
        val drained = drainKnockRows(listOf(row(20, 7), row(21, 8)), after = 19) { bytes ->
            group.processKnock(bytes)
        }
        assertTrue(drained.accepted.isEmpty())
        assertEquals(2, drained.refused)
        assertEquals(21L, drained.cursor)
    }

    @Test
    fun `a refusal in the middle does not stop the rows after it`() {
        val group = FakeGroup(admit = setOf(1, 3))
        val drained = drainKnockRows(listOf(row(1, 1), row(2, 2), row(3, 3)), after = 0) { bytes ->
            group.processKnock(bytes)
        }
        assertEquals(listOf(1L, 3L), drained.accepted.map { it.first.id })
        assertEquals(1, drained.refused)
        assertEquals(3L, drained.cursor)
    }

    @Test
    fun `an empty batch keeps the cursor`() {
        val drained = drainKnockRows<String>(emptyList(), after = 42) { error("never called") }
        assertEquals(42L, drained.cursor)
        assertEquals(0, drained.refused)
    }

    /**
     * ADR-0050 section 2.5a point 7 — knocks are read by an owner or an admin,
     * decided on the ROLE. A plain member now holds a committer rank while it
     * can sweep a device admission, and the predicate takes no rank at all, so
     * that rank cannot turn knock reading on.
     */
    @Test
    fun `knocks are read by the owner and admins only, never by a plain member with a rank`() {
        assertTrue(readsKnocks(Role.OWNER))
        assertTrue(readsKnocks(Role.ADMIN))
        assertFalse(readsKnocks(Role.MEMBER))
        assertFalse(readsKnocks(null))
    }
}
