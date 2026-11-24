// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/group-and-channel-privacy.md` section 8 — the recorded
 * "Asked / Invited ... ago" of a request lasts only while it waits: a person
 * approved, who left and asked again, is dated by the new request.
 */
class KnockOriginLifetimeTest {

    private val prefix = GroupSettingsStore.KNOCK_ORIGIN

    private fun key(joiner: String, account: String = "acct", group: String = "ab12") = "$prefix$joiner:$account:$group"

    @Test
    fun `a re-ask while waiting keeps the first time, a newly kept request replaces it`() {
        assertTrue(knockOriginWrites(held = false, fresh = false))
        assertFalse(knockOriginWrites(held = true, fresh = false))
        assertTrue(knockOriginWrites(held = true, fresh = true))
        assertTrue(knockOriginWrites(held = false, fresh = true))
    }

    @Test
    fun `the origins of requests no longer waiting are dropped, others and other groups kept`() {
        val keys = listOf(
            key("aaaa"),
            key("bbbb"),
            key("aaaa", group = "cd34"),
            key("aaaa", account = "other"),
            "knock-reply:aaaa:acct:ab12",
        )
        val drop = knockOriginKeysToDrop(keys, prefix, "acct", "AB12", waitingHex = setOf("BBBB"))
        assertEquals(listOf(key("aaaa")), drop)
    }

    @Test
    fun `an empty pending set drops every origin of the group`() {
        val keys = listOf(key("aaaa"), key("bbbb"), key("cccc", group = "cd34"))
        assertEquals(listOf(key("aaaa"), key("bbbb")), knockOriginKeysToDrop(keys, prefix, "acct", "ab12", emptySet()))
    }
}
