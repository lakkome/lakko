// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import me.lakko.app.channels.HandleClaimRows
import me.lakko.app.discovery.HandleWriteError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** ADR-0087 section 3 (note of 2026-11-14): a failed New group does not burn
 *  its `@name`; a retry moves the name from an earlier attempt's key. */
class GroupHandleClaimsTest {

    private class Rows : HandleClaimRows {
        val held = HashMap<String, String>()
        override fun get(key: String): String? = held[key]
        override fun put(key: String, value: String) {
            held[key] = value
        }
        override fun remove(key: String) {
            held.remove(key)
        }
    }

    private val claims = GroupHandleClaims(Rows())

    @Test
    fun `attempts are kept newest first and a retry gets the earlier ones`() {
        assertEquals(emptyList<String>(), claims.keep("acct", "team", "g1"))
        assertEquals(listOf("g1"), claims.keep("acct", "@Team", "g2"))
        assertEquals(listOf("g2", "g1"), claims.groupIds("acct", "team"))
        assertTrue(claims.isClaimed("acct", "TEAM"))
        assertFalse(claims.isClaimed("other", "team"))
        assertFalse(claims.isClaimed("acct", "news"))
    }

    @Test
    fun `the number of attempts kept is bounded`() {
        repeat(GroupHandleClaims.MAX_ATTEMPTS + 3) { claims.keep("acct", "team", "g$it") }
        val kept = claims.groupIds("acct", "team")
        assertEquals(GroupHandleClaims.MAX_ATTEMPTS, kept.size)
        assertEquals("g${GroupHandleClaims.MAX_ATTEMPTS + 2}", kept.first())
    }

    @Test
    fun `the group exists or the name is someone else's, and the attempts go`() {
        claims.keep("acct", "team", "g1")
        claims.forget("acct", "Team")
        assertFalse(claims.isClaimed("acct", "team"))
    }

    private val g1 = byteArrayOf(1)
    private val g2 = byteArrayOf(2)
    private val refused = HandleWriteError(401, "statement-refused")

    private fun run(earlier: List<ByteArray>, calls: MutableList<String>, rekey: (ByteArray) -> Unit): String =
        runBlocking {
            claimOrTakeBackGroupHandle(
                earlier,
                claim = { calls.add("claim"); "claimed" },
                rekeyFrom = { id -> calls.add("rekey:${id[0]}"); rekey(id) },
                update = { calls.add("update"); "updated" },
            )
        }

    @Test
    fun `a first attempt is a plain claim`() {
        val calls = mutableListOf<String>()
        assertEquals("claimed", run(emptyList(), calls) {})
        assertEquals(listOf("claim"), calls)
    }

    @Test
    fun `a retry rekeys from the earlier attempt and updates under the new key`() {
        val calls = mutableListOf<String>()
        assertEquals("updated", run(listOf(g1), calls) {})
        assertEquals(listOf("rekey:1", "update"), calls)
    }

    @Test
    fun `a first claim that never landed is a plain claim`() {
        val calls = mutableListOf<String>()
        assertEquals("claimed", run(listOf(g1), calls) { throw HandleWriteError(404, "no-such-handle") })
        assertEquals(listOf("rekey:1", "claim"), calls)
    }

    @Test
    fun `after a lost rekey answer the newest earlier attempt holds the name`() {
        val calls = mutableListOf<String>()
        assertEquals("updated", run(listOf(g2, g1), calls) { if (it[0] == 1.toByte()) throw refused })
        assertEquals(listOf("rekey:2", "update"), calls)
    }

    @Test
    fun `another key's row means the name is held`() {
        val calls = mutableListOf<String>()
        try {
            run(listOf(g2, g1), calls) { throw refused }
            fail("held by another key")
        } catch (e: HandleWriteError) {
            assertEquals("name-taken", e.code)
        }
        assertEquals(listOf("rekey:2", "rekey:1"), calls)
    }
}
