// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The durable group-call records ([GroupCallStore]): codec, round trip, the
 *  max-age drop on load, and per-account isolation. */
class GroupCallStoreTest {

    private val t0 = 1_800_000_000_000L
    private val groupA = "AA".repeat(16)
    private val groupB = "bb".repeat(16)

    private fun record(callFill: Int, startedAtMs: Long = t0) = GroupCallRecord(
        callIdHex = callIdHexOf(ByteArray(16) { callFill.toByte() }),
        audio = true,
        video = true,
        initiatorLeaf = 7u,
        startedAtMs = startedAtMs,
        joinedMembers = mapOf(
            "bob-phone" to GroupCallMember("bob", t0 + 1_000L),
            "bob-laptop" to GroupCallMember("bob", t0 + 1_500L),
            "carol-phone" to GroupCallMember("carol", t0 + 2_000L),
        ),
        lastActivityMs = t0 + 2_000L,
    )

    @Test
    fun `a record round trips through the store`() {
        val store = InMemoryGroupCallStore()
        val rec = record(1)
        store.save("acct", groupA, rec.toRow())

        val seeded = groupCallRecordsToSeed(store.loadAccount("acct"), nowMs = t0 + 5_000L)
        assertEquals(mapOf(groupA.lowercase() to rec), seeded)
    }

    @Test
    fun `saving null removes the record`() {
        val store = InMemoryGroupCallStore()
        store.save("acct", groupA, record(1).toRow())
        store.save("acct", groupA, null)
        assertTrue(store.loadAccount("acct").isEmpty())
    }

    @Test
    fun `records past the maximum age are dropped on load`() {
        val store = InMemoryGroupCallStore()
        store.save("acct", groupA, record(1, startedAtMs = t0).toRow())
        store.save("acct", groupB, record(2, startedAtMs = t0 + 60_000L).toRow())

        val seeded = groupCallRecordsToSeed(store.loadAccount("acct"), nowMs = t0 + GROUP_CALL_MAX_AGE_MS)
        assertEquals(setOf(groupB), seeded.keys)
    }

    @Test
    fun `accounts are isolated and deleteAccount wipes only its own`() {
        val store = InMemoryGroupCallStore()
        store.save("a1", groupA, record(1).toRow())
        store.save("a2", groupA, record(2).toRow())
        store.deleteAccount("a1")
        assertTrue(store.loadAccount("a1").isEmpty())
        assertEquals(1, store.loadAccount("a2").size)
    }

    @Test
    fun `malformed rows decode to nothing`() {
        assertNull(decodeGroupCallRow(null))
        assertNull(decodeGroupCallRow("not json"))
        val bad = record(1).toRow().copy(initiatorLeaf = -1)
        assertNull(bad.toRecord())
        assertNull(record(1).toRow().copy(callIdHex = "zz").toRecord())
        assertEquals(record(1).toRow(), decodeGroupCallRow(encodeGroupCallRow(record(1).toRow())))
    }
}
