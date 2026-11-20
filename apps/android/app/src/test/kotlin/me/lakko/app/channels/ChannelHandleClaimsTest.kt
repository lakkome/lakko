// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** A failed channel create does not burn the handle: a retry reuses the id
 *  the handle was claimed for. */
class ChannelHandleClaimsTest {

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

    private var minted = 0
    private val rows = Rows()
    private val claims = ChannelHandleClaims(rows) { "id${++minted}" }

    @Test
    fun `a retry for the same handle reuses the id, in any case`() {
        val first = claims.idFor("acct", "Trip")
        assertEquals(first, claims.idFor("acct", "trip"))
        assertEquals(1, minted)
    }

    @Test
    fun `the id is kept per account and per handle`() {
        val first = claims.idFor("acct", "trip")
        assertNotEquals(first, claims.idFor("other", "trip"))
        assertNotEquals(first, claims.idFor("acct", "news"))
    }

    @Test
    fun `no handle means a new id every time, and nothing kept`() {
        assertNotEquals(claims.idFor("acct", ""), claims.idFor("acct", ""))
        assertEquals(0, rows.held.size)
    }

    @Test
    fun `once the channel exists, or the name is someone else's, the id is gone`() {
        val first = claims.idFor("acct", "trip")
        claims.forget("acct", "Trip")
        assertNotEquals(first, claims.idFor("acct", "trip"))
    }

    @Test
    fun `a kept name is this account's, not taken, until it is forgotten`() {
        assertFalse(claims.isClaimed("acct", "trip"))
        claims.idFor("acct", "trip")
        assertTrue(claims.isClaimed("acct", "Trip"))
        assertFalse(claims.isClaimed("other", "trip"))
        assertFalse(claims.isClaimed("acct", ""))
        claims.forget("acct", "trip")
        assertFalse(claims.isClaimed("acct", "trip"))
    }
}
